use axum::Router;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use tower_http::cors::{Any, CorsLayer};

use crate::auth::require_token;
use crate::runs::{cancel_run, create_run, get_run};
use crate::state::AppState;

pub fn build_router(state: AppState) -> Router {
    let cors = CorsLayer::new()
        .allow_origin([
            "http://localhost:1420".parse().unwrap(), // Vite dev server (npm run tauri dev)
            "https://tauri.localhost".parse().unwrap(), // production WebView2 origin on Windows
        ])
        .allow_methods(Any)
        .allow_headers(Any);

    let protected = Router::new()
        .route("/status", get(status))
        .route("/runs", post(create_run))
        .route("/runs/{id}", get(get_run))
        .route("/runs/{id}/cancel", post(cancel_run))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            require_token,
        ))
        .with_state(state);

    Router::new()
        .route("/health", get(health))
        .merge(protected)
        .layer(cors)
}

async fn health() -> impl IntoResponse {
    (StatusCode::OK, "ok")
}

async fn status() -> impl IntoResponse {
    (StatusCode::OK, "daemon running")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::Token;
    use crate::runner::FakeCommandRunner;
    use axum::body::Body;
    use axum::http::Request;
    use std::sync::Arc;
    use tower::ServiceExt;

    async fn test_state() -> AppState {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                sqlx::sqlite::SqliteConnectOptions::new()
                    .filename(":memory:")
                    .create_if_missing(true),
            )
            .await
            .unwrap();
        AppState {
            token: Token("test-token".into()),
            pool,
            runner: Arc::new(FakeCommandRunner::default()),
            run_handles: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            run_timeout: crate::state::DEFAULT_RUN_TIMEOUT,
        }
    }

    #[tokio::test]
    async fn health_returns_200_ok() {
        let app = build_router(test_state().await);
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }
}
