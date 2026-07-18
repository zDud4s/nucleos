use axum::{
    extract::{Request, State},
    http::{StatusCode, header},
    middleware::Next,
    response::Response,
};
use rand::RngExt;

use crate::state::AppState;

#[derive(Clone)]
pub struct Token(pub String);

pub fn generate_token() -> String {
    rand::rng()
        .sample_iter(&rand::distr::Alphanumeric)
        .take(32)
        .map(char::from)
        .collect()
}

pub async fn require_token(
    State(state): State<AppState>,
    req: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let header_value = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());

    match header_value {
        Some(v) if v == format!("Bearer {}", state.token.0) => Ok(next.run(req).await),
        _ => Err(StatusCode::UNAUTHORIZED),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::FakeCommandRunner;
    use axum::Router;
    use axum::body::Body;
    use axum::http::Request as HttpRequest;
    use axum::routing::get;
    use std::sync::Arc;
    use tower::ServiceExt;

    async fn test_state(token: &str) -> AppState {
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
            token: Token(token.to_string()),
            pool,
            runner: Arc::new(FakeCommandRunner::default()),
        }
    }

    fn protected_router(state: AppState) -> Router {
        Router::new()
            .route("/secret", get(|| async { "top secret" }))
            .layer(axum::middleware::from_fn_with_state(
                state.clone(),
                require_token,
            ))
            .with_state(state)
    }

    #[tokio::test]
    async fn rejects_missing_token() {
        let app = protected_router(test_state("expected-token").await);
        let response = app
            .oneshot(
                HttpRequest::builder()
                    .uri("/secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn rejects_wrong_token() {
        let app = protected_router(test_state("expected-token").await);
        let response = app
            .oneshot(
                HttpRequest::builder()
                    .uri("/secret")
                    .header("Authorization", "Bearer wrong-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn accepts_correct_token() {
        let app = protected_router(test_state("expected-token").await);
        let response = app
            .oneshot(
                HttpRequest::builder()
                    .uri("/secret")
                    .header("Authorization", "Bearer expected-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }
}
