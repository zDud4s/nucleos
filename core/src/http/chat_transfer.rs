//! Moving a chat between machines, origin side: `preview`, `begin`, `finish` and `abort` under
//! `/assistant/chats/{chat_id}/transfer/`. They sit under `assistant`, the chat API, so an Admin
//! device reaches them like every other chat route (a `/chats/` prefix is refused to every device).
//! The state and its rules are `chats::transfer`; this file maps them onto the wire.

use super::*;
use crate::chats::transfer::{self, Refusal, When};
use crate::chats::transfer_package as package;
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
        .route(
            "/assistant/chats/{chat_id}/transfer/package",
            get(get_transfer_package),
        )
        .route(
            "/assistant/chats/import",
            post(post_chat_import).layer(DefaultBodyLimit::max(IMPORT_BODY_LIMIT)),
        )
}

/// A package holds a bundle, a diff, new files and a transcript, all base64: well past the default.
const IMPORT_BODY_LIMIT: usize = 768 * 1024 * 1024;

fn package_failure(failure: package::Failure) -> Failure {
    match failure {
        package::Failure::Refused(refusal) => refused(refusal),
        package::Failure::Problem(problem) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({ "error": problem })),
        ),
    }
}

#[derive(Deserialize)]
struct TransferQuery {
    transfer_id: String,
}

fn required_transfer_id(id: &str) -> Result<(), Failure> {
    if id.trim().is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "transfer_id is required" })),
        ));
    }
    Ok(())
}

/// The frozen chat as a package: the code, the session, the queue. Needs the standing freeze of
/// `transfer_id` and confirms it alive.
async fn get_transfer_package(
    State(state): State<AppState>,
    Path(chat_id): Path<String>,
    Query(query): Query<TransferQuery>,
) -> Result<Json<package::Package>, Failure> {
    required_transfer_id(&query.transfer_id)?;
    let store = crate::sessions::default_root().ok_or_else(|| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": "this machine has no session store" })),
        )
    })?;
    let live = crate::assistant::chat_move::parked_now(&chat_id);
    package::build(&state.pool, &chat_id, &query.transfer_id, &store, live)
        .await
        .map(Json)
        .map_err(package_failure)
}

/// Unpacks a package on this machine and starts the interrupted turn. Repeating the same
/// `transfer_id` answers `already` and changes nothing.
async fn post_chat_import(
    State(state): State<AppState>,
    Query(query): Query<TransferQuery>,
    Json(body): Json<package::Package>,
) -> Result<Json<Value>, Failure> {
    required_transfer_id(&query.transfer_id)?;
    if body.transfer_id != query.transfer_id {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "the package belongs to another transfer" })),
        ));
    }
    let store = crate::sessions::default_root().ok_or_else(|| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": "this machine has no session store" })),
        )
    })?;
    match package::import(&state.pool, &body, &store)
        .await
        .map_err(package_failure)?
    {
        package::Imported::Already => Ok(Json(json!({ "already": true }))),
        package::Imported::Done { cwd, queued } => {
            if body.session.is_none() {
                // Sessionless arrival: the import already dropped the stored session, this also
                // clears a stale in-memory untrusted-read notice so it does not follow the chat.
                if let Err(error) =
                    crate::assistant::forget_session(&state.pool, &body.chat_id).await
                {
                    tracing::warn!(%error, chat_id = %body.chat_id, "could not reset the imported chat's session");
                }
            }
            if queued > 0 {
                crate::assistant::resume_queue_after_freeze(&state, &body.chat_id);
            }
            Ok(Json(json!({
                "already": false,
                "chat_id": body.chat_id,
                "cwd": cwd.map(|c| c.to_string_lossy().into_owned()),
                "queued": queued,
            })))
        }
    }
}

type Failure = (StatusCode, Json<Value>);

fn refused(refusal: Refusal) -> Failure {
    let status = match refusal {
        Refusal::NoSuchChat => StatusCode::NOT_FOUND,
        Refusal::AlreadyFrozen
        | Refusal::Moved(_)
        | Refusal::NotFrozenForThis
        | Refusal::OriginChangedAfterExpiry
        | Refusal::AlreadyHere => StatusCode::CONFLICT,
        Refusal::Store(ref error) => {
            tracing::warn!(%error, "a chat transfer step failed");
            StatusCode::INTERNAL_SERVER_ERROR
        }
    };
    let body = match refusal {
        Refusal::OriginChangedAfterExpiry => {
            json!({ "refusal": "origin_changed_after_expiry", "detail": refusal.message() })
        }
        _ => json!({ "error": refusal.message() }),
    };
    (status, Json(body))
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
    let parked = crate::assistant::chat_move::parked_now(&chat_id);
    transfer::preview(&state.pool, &chat_id, running, &parked)
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
    to_name: Option<String>,
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
    let name = body.to_name.as_deref().unwrap_or(&body.to);
    let destination = json!({ "id": body.to, "name": name }).to_string();
    transfer::finish_at(
        &state.pool,
        &chat_id,
        &body.transfer_id,
        &destination,
        transfer::unix_now(),
    )
    .await
    .map_err(refused)?;
    // The session lives on the other machine now; a process kept here would only idle on it.
    crate::assistant::evict_live(&chat_id);
    package::forget(&chat_id);
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
    required_transfer_id(&body.transfer_id)?;
    crate::assistant::chat_move::abort_and_resume(&state, &chat_id, &body.transfer_id)
        .await
        .map_err(store)?;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origin_changed_after_expiry_is_named_for_the_dialog() {
        let (status, Json(body)) = refused(Refusal::OriginChangedAfterExpiry);

        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(
            body["refusal"].as_str(),
            Some("origin_changed_after_expiry")
        );
    }

    #[tokio::test]
    async fn post_transfer_abort_after_expiry_clears_the_remembered_move() {
        let state = super::super::tests::test_state().await;
        sqlx::query("INSERT INTO chats (chat_id, brain, created_at) VALUES ('c1', 'cloud', 'now')")
            .execute(&state.pool)
            .await
            .unwrap();
        crate::chats::transfer::freeze_at(&state.pool, "c1", "t-1", 1)
            .await
            .unwrap();
        package::remember("c1", true, vec![]);

        let status = post_transfer_abort(
            State(state.clone()),
            Path("c1".to_string()),
            Json(AbortBody {
                transfer_id: "t-1".to_string(),
            }),
        )
        .await;

        assert!(matches!(status, Ok(StatusCode::NO_CONTENT)));
        assert!(!package::is_remembered("c1"));
    }

    #[tokio::test]
    async fn post_transfer_abort_rejects_an_empty_transfer_id() {
        let state = super::super::tests::test_state().await;

        let result = post_transfer_abort(
            State(state),
            Path("c1".to_string()),
            Json(AbortBody {
                transfer_id: " ".to_string(),
            }),
        )
        .await;

        assert!(matches!(result, Err((StatusCode::BAD_REQUEST, _))));
    }
}
