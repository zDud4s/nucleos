//! Conductor HTTP routes.

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::post;
use axum::{Json, Router};
use chrono::Utc;
use serde::Deserialize;

use crate::state::AppState;

#[derive(Debug, Deserialize)]
struct FeedRequest {
    kind: String,
    session_id: Option<String>,
    cwd: Option<String>,
    tool_name: Option<String>,
    reason: Option<String>,
}

/// Routes supplied to the daemon's protected router.
pub fn routes() -> Router<AppState> {
    Router::new().route("/conductor/feed", post(post_feed))
}

/// Records a hook's observe-only refusal without making the hook wait for routing.
async fn post_feed(
    State(state): State<AppState>,
    Json(request): Json<FeedRequest>,
) -> Result<StatusCode, StatusCode> {
    let session_id = request.session_id.filter(|id| !id.is_empty()).ok_or(StatusCode::BAD_REQUEST)?;
    let subject = format!("session:{session_id}");
    let summary = format!(
        "{}{}{}",
        request.tool_name.unwrap_or_else(|| "tool".to_owned()),
        request.reason.map(|reason| format!(": {reason}")).unwrap_or_default(),
        request.cwd.map(|cwd| format!(" ({cwd})")).unwrap_or_default(),
    );
    sqlx::query("INSERT INTO feed (kind, summary, subject, created_at) VALUES (?, ?, ?, ?)")
        .bind(request.kind)
        .bind(summary)
        .bind(subject)
        .bind(Utc::now().to_rfc3339())
        .execute(&state.pool)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(StatusCode::ACCEPTED)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::Router;
    use serde_json::{Value, json};
    use tower::ServiceExt;

    use super::routes;
    use crate::state::AppState;

    async fn state() -> AppState {
        crate::execution::runs::testkit::test_state_with_runner(
            Some(Duration::from_secs(5)),
            Duration::from_secs(600),
        )
        .await
        .0
    }

    fn app(state: &AppState) -> Router {
        routes().with_state(state.clone())
    }

    async fn request(app: &Router, method: &str, uri: &str, body: Value) -> (StatusCode, Value) {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(uri)
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, serde_json::from_slice(&body).unwrap_or(Value::Null))
    }

    #[tokio::test]
    async fn liveness_route_lists_open_episodes() {
        let state = state().await;
        sqlx::query(
            "INSERT INTO feed (kind, summary, subject, created_at) \
             VALUES ('session_stalled', 'no transcript progress', 'session:session-1', '2026-10-30T00:00:00Z')",
        )
        .execute(&state.pool)
        .await
        .unwrap();

        let (status, episodes) = request(&app(&state), "GET", "/conductor/liveness", Value::Null).await;

        assert_eq!(status, StatusCode::OK, "{episodes:?}");
        assert!(episodes.as_array().unwrap().iter().any(|episode| {
            episode["kind"] == "session_stalled" && episode["subject"] == "session:session-1"
        }));
    }

    #[tokio::test]
    async fn feed_route_accepts_a_hook_line() {
        let state = state().await;

        let (status, _) = request(
            &app(&state),
            "POST",
            "/conductor/feed",
            json!({
                "kind": "conductor_would_deny",
                "session_id": "session-1",
                "tool_name": "Edit",
                "reason": "wrapping",
                "cwd": "C:/repo"
            }),
        )
        .await;

        assert_eq!(status, StatusCode::ACCEPTED);
        let row: (String, String) = sqlx::query_as(
            "SELECT kind, subject FROM feed WHERE subject = 'session:session-1'",
        )
        .fetch_one(&state.pool)
        .await
        .unwrap();
        assert_eq!(row.0, "conductor_would_deny");
        assert_eq!(row.1, "session:session-1");
    }
}
