//! Moving a chat between machines, origin side: `preview`, `begin`, `finish` and `abort` under
//! `/assistant/chats/{chat_id}/transfer/`. They sit under `assistant`, the chat API, so an Admin
//! device reaches them like every other chat route (a `/chats/` prefix is refused to every device).
//! The state and its rules are `chats::transfer`; this file maps them onto the wire.

use super::*;
use crate::chats::transfer::{self, Refusal, When};
use serde_json::{Value, json};

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/assistant/chats/{chat_id}/transfer/preview",
            get(get_transfer_preview),
        )
        .route(
            "/assistant/chats/{chat_id}/transfer/begin",
            post(post_transfer_begin),
        )
        .route(
            "/assistant/chats/{chat_id}/transfer/finish",
            post(post_transfer_finish),
        )
        .route(
            "/assistant/chats/{chat_id}/transfer/abort",
            post(post_transfer_abort),
        )
}

type Failure = (StatusCode, Json<Value>);

fn refused(refusal: Refusal) -> Failure {
    let status = match refusal {
        Refusal::NoSuchChat => StatusCode::NOT_FOUND,
        Refusal::AlreadyFrozen | Refusal::Moved(_) | Refusal::NotFrozenForThis => {
            StatusCode::CONFLICT
        }
        Refusal::Store(ref error) => {
            tracing::warn!(%error, "a chat transfer step failed");
            StatusCode::INTERNAL_SERVER_ERROR
        }
    };
    (status, Json(json!({ "error": refusal.message() })))
}

fn store(error: sqlx::Error) -> Failure {
    refused(Refusal::Store(error))
}

/// What the dialog shows before the person confirms: what a move now would end, what stays behind,
/// and whether the destination can take the code.
async fn get_transfer_preview(
    State(state): State<AppState>,
    Path(chat_id): Path<String>,
) -> Result<Json<transfer::Preview>, Failure> {
    let running = crate::assistant::is_busy(&chat_id);
    transfer::preview(&state.pool, &chat_id, running)
        .await
        .map(Json)
        .map_err(refused)
}

#[derive(Deserialize)]
struct BeginBody {
    when: When,
    transfer_id: String,
}

/// Freezes the chat; with `after_turn` the answer comes when the turn in flight has ended.
async fn post_transfer_begin(
    State(state): State<AppState>,
    Path(chat_id): Path<String>,
    Json(body): Json<BeginBody>,
) -> Result<Json<crate::assistant::chat_move::Begun>, Failure> {
    if body.transfer_id.trim().is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "transfer_id is required" })),
        ));
    }
    crate::assistant::chat_move::begin(&state, &chat_id, body.when, &body.transfer_id)
        .await
        .map(Json)
        .map_err(refused)
}

#[derive(Deserialize)]
struct FinishBody {
    transfer_id: String,
    to: String,
}

/// The destination has the chat: this copy becomes read-only and points there.
async fn post_transfer_finish(
    State(state): State<AppState>,
    Path(chat_id): Path<String>,
    Json(body): Json<FinishBody>,
) -> Result<StatusCode, Failure> {
    if body.to.trim().is_empty() || body.transfer_id.trim().is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "transfer_id and to are required" })),
        ));
    }
    transfer::finish_at(
        &state.pool,
        &chat_id,
        &body.transfer_id,
        &body.to,
        transfer::unix_now(),
    )
    .await
    .map_err(refused)?;
    // The session lives on the other machine now; a process kept here would only idle on it.
    crate::assistant::evict_live(&chat_id);
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct AbortBody {
    transfer_id: String,
}

/// Any failure before `finish`: the chat goes on working here. Safe to repeat.
async fn post_transfer_abort(
    State(state): State<AppState>,
    Path(chat_id): Path<String>,
    Json(body): Json<AbortBody>,
) -> Result<StatusCode, Failure> {
    crate::assistant::chat_move::abort_and_resume(&state, &chat_id, &body.transfer_id)
        .await
        .map_err(store)?;
    Ok(StatusCode::NO_CONTENT)
}
