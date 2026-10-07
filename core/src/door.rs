//! The pieces of the HTTP door that the rest of the daemon reaches for.
//!
//! `http.rs` is the binary's: it holds the router and every route, and it is the file that
//! changes most often (27% of the core's commits in the 30 days to 2026-10-06), so it lives in
//! the binary crate where an edit to it recompiles only the binary. What is here is what the
//! library itself calls or tests against — the uncancellable spawn three pillars use, the files
//! root two of them share, and the knowledge door the MCP toolbox's own tests drive end to end.
//! Nothing here is new: it was moved out of `http.rs` as it was.

use axum::Json;
use axum::extract::{Extension, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use serde::Deserialize;

use crate::state::AppState;

/// The folder root, or a refusal when startup could not create it.
///
/// `pub(crate)` because a second pillar with a loop of its own now reads the same root, and the one
/// thing worth sharing is the 503: an installation with no files folder must answer the same way
/// whichever route asked. The field itself lives on `AppState` rather than in any one pillar's
/// runtime — see the doc there for why it stopped being the mail pillar's.
pub fn files_root(state: &AppState) -> Result<&std::path::Path, StatusCode> {
    state
        .files_root
        .as_deref()
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)
}

/// A refusal, named so the caller can answer it.
///
/// The status code is the coarse signal and stays honest for anything between here and the caller;
/// the slug is the fine one, because this route has more refusals than HTTP has codes that fit
/// them. Four, against three — 403 is spent by `auth.rs` on token level and would read as a
/// rejected token, which is the one thing this never is.
///
/// A slug and not the sentence, for the reason `assistant.rs` records around `NO_LOCAL_MODEL`: a
/// refusal recognised by its prose stops being recognised the day somebody improves the wording,
/// and it fails silently — a deliberate refusal starts reading as a crash. And the sentence is not
/// this crate's to write anyway. What undoes a pause is `/retomar`, a Telegram command; the
/// núcleo says which refusal happened and whoever is talking to the person says what to do about
/// it, in the language they are being spoken to in.
pub fn refusal(status: StatusCode, name: &'static str) -> (StatusCode, Json<serde_json::Value>) {
    (status, Json(serde_json::json!({ "refusal": name })))
}

/// The run asking for this relay, read the one place a caller cannot simply state it: the header
/// `daemon_client::RUN_ID_HEADER` puts on every request a run's own tool calls make, set from an
/// environment variable that process has no tool able to read or alter — see that constant's own
/// doc, and `DaemonClient::send_to_chat`'s. Absent or unparseable answers `None` rather than a
/// guess: a relay with no run behind it has nothing for `relay::admit` to walk a chain from, so the
/// request is refused rather than attributed to whichever run the caller happened to be.
pub fn sending_run_id_of(headers: &axum::http::HeaderMap) -> Option<i64> {
    headers
        .get(crate::daemon_client::RUN_ID_HEADER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<i64>().ok())
}

/// Runs `work` in its own task so a request that goes away cannot abandon it half-done.
///
/// A client that disconnects cancels the request it was making, and the handler's future is dropped
/// — the same mechanism `abort()` uses on a run's task, with the same consequence: everything
/// sequenced after the drop point is silently never done. That is only a missing reply when the
/// handler reads; when it mutates durable state across awaits, it strands the half it had finished,
/// and the half-states here (a `running` or `awaiting_approval` worktree run) hold one of their
/// project's concurrency slots until something notices (`concurrency.rs`).
///
/// Awaiting the JoinHandle leaves the response exactly as it was; dropping a JoinHandle only
/// detaches its task, so the work still runs to the end. A panicking task becomes a 500 — the task
/// is gone, so there is no result left to return.
pub async fn uncancellable<T, F>(work: F) -> Result<T, StatusCode>
where
    F: std::future::Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    tokio::spawn(work)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

/// Scope comes from the run, and a `project_id` key is accepted and ignored.
#[derive(serde::Deserialize)]
pub struct ProposeKnowledgeRequest {
    kind: String,
    title: String,
    body: String,
    /// Why this is worth telling every later run. Carried onto the proposal, because a person
    /// deciding at a glance needs the argument beside the text and not a screen away from it.
    reasoning: Option<String>,
    /// The refinement this one replaces, if it is a correction of something already in force.
    ///
    /// Optional, and the difference matters: without it the layer only grows, and the answer to
    /// "this note is wrong now" is a second note contradicting the first with both still in force.
    supersedes: Option<i64>,
}

#[derive(Deserialize)]
pub struct RecallRequest {
    query: String,
    layer: Option<String>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FindingRequest {
    fact: String,
    evidence: serde_json::Value,
}

/// The run-key-only door for one evidenced working fact in the caller's own live job.
pub async fn post_finding(
    State(state): State<AppState>,
    Extension(scope): Extension<crate::auth::Scope>,
    Json(request): Json<FindingRequest>,
) -> Result<(StatusCode, Json<serde_json::Value>), axum::response::Response> {
    let crate::auth::Scope::Run(run_id) = scope else {
        return Err((StatusCode::FORBIDDEN, "only a run may leave a finding").into_response());
    };
    let knowledge_id =
        crate::knowledge::note_finding(&state.pool, run_id, &request.fact, &request.evidence)
            .await
            .map_err(|error| match error {
                crate::knowledge::FindingError::EmptyFact
                | crate::knowledge::FindingError::FactTooLong
                | crate::knowledge::FindingError::NoEvidence
                | crate::knowledge::FindingError::EvidenceTooLong => {
                    (StatusCode::BAD_REQUEST, error.to_string()).into_response()
                }
                crate::knowledge::FindingError::NoJob
                | crate::knowledge::FindingError::JobEnded => {
                    (StatusCode::CONFLICT, error.to_string()).into_response()
                }
                crate::knowledge::FindingError::TooMany => {
                    (StatusCode::TOO_MANY_REQUESTS, error.to_string()).into_response()
                }
                crate::knowledge::FindingError::Db(error) => {
                    tracing::warn!(%error, run_id, "writing a finding failed");
                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "the finding could not be written",
                    )
                        .into_response()
                }
            })?;
    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({"knowledge_id": knowledge_id})),
    ))
}

/// The one door a run declares through.
///
/// It still goes through the proposal, rather than inserting an `active` row: the review trail is
/// what makes the layer safe to have at all, and a second way in that skipped it would be the way
/// everything eventually got written.
pub async fn post_knowledge(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(request): Json<ProposeKnowledgeRequest>,
) -> Result<(StatusCode, Json<serde_json::Value>), axum::response::Response> {
    // These are the relay's named slugs for the same header, rather than sentences, for the reason
    // on `refusal`. A model reads the body verbatim through `declare_refinement`. By owner scope,
    // 2026-09-24, every other refusal in this handler stays prose.
    let Some(origin_run_id) = sending_run_id_of(&headers) else {
        return Err(refusal(StatusCode::BAD_REQUEST, "missing_run_id").into_response());
    };
    let kind = crate::knowledge::Kind::parse(request.kind.trim()).ok_or_else(|| {
        (
            StatusCode::BAD_REQUEST,
            "kind must be one of prompt, memory, skill, subagent".to_owned(),
        )
            .into_response()
    })?;
    let title = request.title.trim();
    let body = request.body.trim();
    // A refinement with no words is an empty heading in every later prompt, for ever.
    if title.is_empty() || body.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "a refinement needs both a title and a body".to_owned(),
        )
            .into_response());
    }
    let project_id = sqlx::query_scalar::<_, Option<String>>(
        "SELECT project_id FROM runs WHERE id = ?",
    )
    .bind(origin_run_id)
    .fetch_optional(&state.pool)
    .await
    .map_err(|error| {
        tracing::warn!(%error, run_id = origin_run_id, "reading a declaration's run scope failed");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "the refinement's scope could not be determined".to_owned(),
        )
            .into_response()
    })?
    .ok_or_else(|| refusal(StatusCode::BAD_REQUEST, "unknown_sender").into_response())?;
    let (knowledge_id, proposal_id) = crate::knowledge::propose(
        &state.pool,
        crate::knowledge::Declaration {
            project_id: project_id.as_deref(),
            // Off the header and never off the body: `RUN_ID_HEADER` is set from an environment
            // variable the run's own tools have nothing able to read or alter, so a run can name
            // itself and cannot name anybody else.
            origin_run_id: Some(origin_run_id),
            kind,
            title,
            body,
            reasoning: request
                .reasoning
                .as_deref()
                .unwrap_or("declared by a run with no reason given"),
            supersedes: request.supersedes,
        },
    )
    .await
    // Which precondition failed, rather than a bare status: a caller told only "409" has to guess
    // between "that id is not there" and "that id is not yours", and the two have different fixes.
    .map_err(|error| match error {
        crate::knowledge::ProposeError::UnknownPredecessor(_) => {
            (StatusCode::NOT_FOUND, error.to_string()).into_response()
        }
        crate::knowledge::ProposeError::ForeignPredecessor(_) => {
            (StatusCode::CONFLICT, error.to_string()).into_response()
        }
        crate::knowledge::ProposeError::Db(error) => {
            tracing::warn!(%error, "proposing a refinement failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "the refinement could not be written".to_owned(),
            )
                .into_response()
        }
    })?;
    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({
            "knowledge_id": knowledge_id,
            "proposal_id": proposal_id,
        })),
    ))
}

pub async fn recall_knowledge(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(request): Json<RecallRequest>,
) -> Result<Json<Vec<crate::brief::Recalled>>, axum::response::Response> {
    let query = request.query.trim();
    if query.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "recall needs a query").into_response());
    }
    let layer = match request.layer.as_deref().map(str::trim) {
        None => None,
        Some(layer) => match crate::knowledge::Layer::parse(layer) {
            Some(layer) => Some(layer),
            None => {
                return Err((
                    StatusCode::BAD_REQUEST,
                    "layer must be one of semantic, episodic, procedural",
                )
                    .into_response());
            }
        },
    };
    if layer == Some(crate::knowledge::Layer::Working) {
        return Err((
            StatusCode::BAD_REQUEST,
            "the working layer is never recalled: it reaches a node only through its briefing",
        )
            .into_response());
    }

    let scope = match sending_run_id_of(&headers) {
        None => crate::knowledge::Scope::Machine,
        Some(run_id) => {
            let project_id =
                sqlx::query_scalar::<_, Option<String>>("SELECT project_id FROM runs WHERE id = ?")
                    .bind(run_id)
                    .fetch_optional(&state.pool)
                    .await
                    .map_err(|error| {
                        tracing::warn!(%error, run_id, "reading a recall's run scope failed");
                        (
                            StatusCode::INTERNAL_SERVER_ERROR,
                            "recall's scope could not be determined",
                        )
                            .into_response()
                    })?
                    .ok_or_else(|| {
                        refusal(StatusCode::BAD_REQUEST, "unknown_sender").into_response()
                    })?;
            project_id.map_or(
                crate::knowledge::Scope::Machine,
                crate::knowledge::Scope::Project,
            )
        }
    };

    crate::brief::recall(&state.pool, &scope, query, layer)
        .await
        .map(Json)
        .map_err(|error| {
            tracing::warn!(%error, "recalling approved knowledge failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "known facts could not be recalled",
            )
                .into_response()
        })
}
