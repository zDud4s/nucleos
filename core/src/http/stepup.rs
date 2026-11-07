//! HTTP doors for a native device's browser step-up ticket.

use super::*;
use crate::auth::RemoteDevice;

const BODY_LIMIT: usize = 4 * 1024;

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/stepup/ticket",
            post(create_ticket).layer(DefaultBodyLimit::max(BODY_LIMIT)),
        )
        .route("/stepup/status", get(stepup_status))
        .route("/remote/step-up/ticket/{ticket}", get(ticket_info))
        .route(
            "/remote/step-up/ticket/consume",
            post(consume_ticket).layer(DefaultBodyLimit::max(BODY_LIMIT)),
        )
}

fn ticket_refusal(error: crate::remote::stepup::TicketError) -> axum::response::Response {
    use crate::remote::stepup::TicketError as E;
    match error {
        E::NotOwner => refusal(StatusCode::FORBIDDEN, "not_an_owner").into_response(),
        E::InvalidAction => {
            refusal(StatusCode::BAD_REQUEST, "invalid_stepup_action").into_response()
        }
        E::Invalid => refusal(StatusCode::GONE, "stepup_ticket_invalid").into_response(),
        E::Storage(error) => {
            tracing::warn!("step-up ticket storage failed: {error}");
            refusal(StatusCode::INTERNAL_SERVER_ERROR, "storage_failed").into_response()
        }
    }
}

#[derive(Deserialize)]
struct TicketBody {
    action: String,
}

/// The device identity comes from the session middleware, never from the request body.
async fn create_ticket(
    State(state): State<AppState>,
    device: Option<Extension<RemoteDevice>>,
    Json(body): Json<TicketBody>,
) -> impl IntoResponse {
    let Some(Extension(device)) = device else {
        return refusal(StatusCode::FORBIDDEN, "not_a_remote_device").into_response();
    };
    let Ok(action) = body.action.parse() else {
        return ticket_refusal(crate::remote::stepup::TicketError::InvalidAction);
    };
    match crate::remote::stepup::create(
        &state.pool,
        device.id,
        device.owner,
        action,
        chrono::Utc::now(),
    )
    .await
    {
        Ok(ticket) => (StatusCode::CREATED, Json(ticket)).into_response(),
        Err(error) => ticket_refusal(error),
    }
}

/// A paired app polls only its own session's stamp; no device list exposes this state.
async fn stepup_status(device: Option<Extension<RemoteDevice>>) -> impl IntoResponse {
    let Some(Extension(device)) = device else {
        return refusal(StatusCode::FORBIDDEN, "not_a_remote_device").into_response();
    };
    Json(serde_json::json!({ "stepped_up": device.stepped_up })).into_response()
}

async fn ticket_info(
    State(state): State<AppState>,
    Path(ticket): Path<String>,
) -> impl IntoResponse {
    match crate::remote::stepup::info(&state.pool, &ticket, chrono::Utc::now()).await {
        Ok(info) => Json(info).into_response(),
        Err(error) => ticket_refusal(error),
    }
}

#[derive(Deserialize)]
struct ConsumeBody {
    ticket: String,
}

async fn consume_ticket(
    State(state): State<AppState>,
    Json(body): Json<ConsumeBody>,
) -> impl IntoResponse {
    match crate::remote::stepup::consume(&state.pool, &body.ticket, chrono::Utc::now()).await {
        Ok(()) => Json(serde_json::json!({ "stepped_up": true, "window_seconds": crate::remote::STEP_UP_WINDOW.num_seconds() })).into_response(),
        Err(error) => ticket_refusal(error),
    }
}
