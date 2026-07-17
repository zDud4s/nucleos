use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::{Router, routing::get};
use tower_http::cors::{Any, CorsLayer};

pub fn build_router() -> Router {
    let cors = CorsLayer::new()
        .allow_origin([
            "http://localhost:1420".parse().unwrap(),
            "https://tauri.localhost".parse().unwrap(),
        ])
        .allow_methods(Any)
        .allow_headers(Any);

    Router::new().route("/health", get(health)).layer(cors)
}

async fn health() -> impl IntoResponse {
    (StatusCode::OK, "ok")
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    #[tokio::test]
    async fn health_returns_200_ok() {
        let app = build_router();
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
