//! stub: tree cost HTTP routes

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
    async fn cost_route_reports_spent() {
        let state = state().await;
        sqlx::query(
            "INSERT INTO runs (id, prompt, status, mode, cost_usd, created_at) \
             VALUES (901, 'costed run', 'completed', 'shadow', 2.5, '2026-10-30T00:00:00Z')",
        )
        .execute(&state.pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO work_nodes (id, root_id, depth, kind, title, goal, status, run_id, created_at, updated_at) \
             VALUES ('node-1', 'tree-1', 0, 'root', 'title', 'goal', 'done', '901', \
                     '2026-10-30T00:00:00Z', '2026-10-30T00:00:00Z')",
        )
        .execute(&state.pool)
        .await
        .unwrap();

        let (status, cost) = request(&app(&state), "GET", "/trees/tree-1/cost", Value::Null).await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(cost["spent_usd"], 2.5, "{cost:?}");
    }

    #[tokio::test]
    async fn budget_put_roundtrips() {
        let state = state().await;
        let app = app(&state);

        let (status, saved) = request(
            &app,
            "PUT",
            "/trees/tree-1/budget",
            json!({ "limit_usd": 12.5 }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{saved:?}");
        assert_eq!(saved["limit_usd"], 12.5);
        let (status, cost) = request(&app, "GET", "/trees/tree-1/cost", Value::Null).await;
        assert_eq!(status, StatusCode::OK, "{cost:?}");
        assert_eq!(cost["budget"]["limit_usd"], 12.5);
    }

    #[tokio::test]
    async fn cache_report_route_with_no_data() {
        let state = state().await;

        let (status, report) = request(&app(&state), "GET", "/cost/cache-report", Value::Null).await;

        assert_eq!(status, StatusCode::OK, "{report:?}");
        assert!(report["effective_ttl"].is_null(), "{report:?}");
    }

    #[tokio::test]
    async fn wait_advice_route_returns_the_action() {
        let state = state().await;

        let (status, advice) = request(&app(&state), "GET", "/cost/wait-advice", Value::Null).await;

        assert_eq!(status, StatusCode::OK, "{advice:?}");
        assert_eq!(advice["action"], "park", "{advice:?}");
    }
}
