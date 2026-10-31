//! stub: spec backlog HTTP routes

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
    async fn scan_then_list_specs() {
        let state = state().await;
        let app = app(&state);

        let (status, _) = request(&app, "POST", "/specs/scan", json!({})).await;
        assert_eq!(status, StatusCode::OK);
        let (status, specs) = request(&app, "GET", "/specs", Value::Null).await;

        assert_eq!(status, StatusCode::OK);
        assert!(specs.is_array(), "{specs:?}");
    }

    #[tokio::test]
    async fn extract_route_returns_202() {
        let state = state().await;
        sqlx::query(
            "INSERT INTO specs (id, path, title, content_sha, created_at, updated_at) \
             VALUES ('spec-1', 'C:/spec.md', 'Spec', 'sha', '2026-10-30T00:00:00Z', '2026-10-30T00:00:00Z')",
        )
        .execute(&state.pool)
        .await
        .unwrap();

        let (status, _) = request(&app(&state), "POST", "/specs/spec-1/extract", json!({})).await;

        // Extraction is asynchronous; this route must not wait for its run.
        assert_eq!(status, StatusCode::ACCEPTED);
    }

    #[tokio::test]
    async fn decisions_route_lists_spec_questions() {
        let state = state().await;
        sqlx::query(
            "INSERT INTO spec_questions (id, spec_id, question, created_at) \
             VALUES ('question-1', 'spec-1', 'Which budget?', '2026-10-30T00:00:00Z')",
        )
        .execute(&state.pool)
        .await
        .unwrap();

        let (status, decisions) = request(&app(&state), "GET", "/decisions", Value::Null).await;

        assert_eq!(status, StatusCode::OK);
        assert!(decisions.as_array().unwrap().iter().any(|decision| {
            decision["id"] == "question-1" && decision["question"] == "Which budget?"
        }));
    }
}
