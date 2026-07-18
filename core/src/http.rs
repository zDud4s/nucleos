use axum::Json;
use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use serde::Deserialize;
use tower_http::cors::{Any, CorsLayer};

use crate::auth::require_token;
use crate::autopilot::{self, ActivationError, Mode};
use crate::feed::{self, FeedEntry};
use crate::hooks::pretooluse_decision;
use crate::runs::{cancel_run, create_run, get_run};
use crate::shadow::{self, ClassTally, ShadowDecision};
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
        .route(
            "/autopilot/state",
            get(get_autopilot_state).post(post_autopilot_state),
        )
        .route("/autopilot/kill", post(post_autopilot_kill))
        .route("/feed", get(get_feed))
        .route("/runs", post(create_run))
        .route("/runs/{id}", get(get_run))
        .route("/runs/{id}/cancel", post(cancel_run))
        .route("/shadow-decisions", get(get_unreviewed_shadow_decisions))
        .route("/shadow-decisions/{id}/verdict", post(post_shadow_verdict))
        .route("/scoreboard", get(get_scoreboard))
        .route("/hooks/pretooluse-decision", post(pretooluse_decision))
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

#[derive(Deserialize)]
struct ProjectQuery {
    project_id: String,
}

#[derive(Deserialize)]
struct FeedQuery {
    project_id: Option<String>,
}

#[derive(Deserialize)]
struct VerdictRequest {
    verdict: String,
}

#[derive(Deserialize)]
struct AutopilotStateRequest {
    project_id: String,
    mode: String,
    project_root: Option<String>,
}

#[derive(serde::Serialize)]
struct AutopilotStateResponse {
    project_id: String,
    mode: Mode,
}

#[derive(Deserialize)]
struct AutopilotKillRequest {
    engaged: bool,
}

async fn get_autopilot_state(
    State(state): State<AppState>,
    Query(query): Query<ProjectQuery>,
) -> Result<Json<AutopilotStateResponse>, StatusCode> {
    let mode = autopilot::project_mode(&state.pool, &query.project_id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(AutopilotStateResponse {
        project_id: query.project_id,
        mode,
    }))
}

async fn post_autopilot_state(
    State(state): State<AppState>,
    Json(body): Json<AutopilotStateRequest>,
) -> Result<Json<AutopilotStateResponse>, StatusCode> {
    let mode = Mode::from_db_str(&body.mode).ok_or(StatusCode::BAD_REQUEST)?;
    let project_root = body.project_root.as_deref().map(std::path::Path::new);
    autopilot::set_project_mode(&state.pool, &body.project_id, mode, project_root)
        .await
        .map_err(activation_status)?;
    let mode = autopilot::project_mode(&state.pool, &body.project_id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(AutopilotStateResponse {
        project_id: body.project_id,
        mode,
    }))
}

async fn post_autopilot_kill(
    State(state): State<AppState>,
    Json(body): Json<AutopilotKillRequest>,
) -> Result<StatusCode, StatusCode> {
    autopilot::set_kill_switch(&state.pool, body.engaged)
        .await
        .map(|()| StatusCode::NO_CONTENT)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

fn activation_status(error: ActivationError) -> StatusCode {
    match error {
        ActivationError::ActiveReserved => StatusCode::CONFLICT,
        ActivationError::ProjectRootRequired
        | ActivationError::NotOnboarded
        | ActivationError::HookNotRegistered => StatusCode::UNPROCESSABLE_ENTITY,
        ActivationError::Database(_) => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

async fn get_feed(
    State(state): State<AppState>,
    Query(query): Query<FeedQuery>,
) -> Result<Json<Vec<FeedEntry>>, StatusCode> {
    feed::list_feed(&state.pool, query.project_id.as_deref(), 50)
        .await
        .map(Json)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

async fn get_unreviewed_shadow_decisions(
    State(state): State<AppState>,
    Query(query): Query<ProjectQuery>,
) -> Result<Json<Vec<ShadowDecision>>, StatusCode> {
    shadow::list_unreviewed(&state.pool, &query.project_id)
        .await
        .map(Json)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

async fn post_shadow_verdict(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(body): Json<VerdictRequest>,
) -> Result<StatusCode, StatusCode> {
    if !matches!(body.verdict.as_str(), "approve" | "reject") {
        return Err(StatusCode::BAD_REQUEST);
    }

    match shadow::set_verdict(&state.pool, id, &body.verdict).await {
        Ok(true) => Ok(StatusCode::NO_CONTENT),
        Ok(false) => Err(StatusCode::NOT_FOUND),
        Err(_) => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

async fn get_scoreboard(
    State(state): State<AppState>,
    Query(query): Query<ProjectQuery>,
) -> Result<Json<Vec<ClassTally>>, StatusCode> {
    shadow::scoreboard(&state.pool, &query.project_id)
        .await
        .map(Json)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
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
