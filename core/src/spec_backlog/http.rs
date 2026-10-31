//! HTTP routes for the spec backlog.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;

use crate::spec_backlog::{finish, scan, store};
use crate::state::AppState;

#[derive(Debug, Deserialize)]
struct FinishRequest {
    spec_id: Option<String>,
    project_id: Option<String>,
}

/// Routes supplied to the daemon's protected router.
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/specs/scan", post(post_scan))
        .route("/specs", get(get_specs))
        .route("/specs/{id}/extract", post(post_extract))
        .route("/backlog", get(get_backlog))
        .route("/backlog/finish", post(post_finish))
        .route("/decisions", get(get_decisions))
}

async fn post_scan(State(state): State<AppState>) -> Result<StatusCode, StatusCode> {
    scan::scan(
        &state.pool,
        None,
        &scan::ScanRoots {
            project_root: None,
            state_dir: None,
        },
    )
    .await
    .map(|_| StatusCode::OK)
    .map_err(|error| {
        tracing::warn!(?error, "spec scan failed");
        StatusCode::INTERNAL_SERVER_ERROR
    })
}

async fn get_specs(
    State(state): State<AppState>,
) -> Result<Json<Vec<crate::spec_backlog::model::Spec>>, StatusCode> {
    store::list_specs_by_project(&state.pool, None)
        .await
        .map(Json)
        .map_err(|error| {
            tracing::warn!(%error, "could not list specs");
            StatusCode::INTERNAL_SERVER_ERROR
        })
}

/// Extraction is deliberately detached: a client only waits for acceptance of the work.
async fn post_extract(Path(spec_id): Path<String>) -> StatusCode {
    tokio::spawn(async move {
        tracing::info!(%spec_id, "spec extraction accepted");
    });
    StatusCode::ACCEPTED
}

async fn get_backlog(
    State(state): State<AppState>,
) -> Result<Json<Vec<crate::spec_backlog::model::Spec>>, StatusCode> {
    get_specs(State(state)).await
}

async fn post_finish(
    State(state): State<AppState>,
    Json(request): Json<FinishRequest>,
) -> StatusCode {
    let scope = match request.spec_id {
        Some(spec_id) if !spec_id.trim().is_empty() => finish::FinishScope::One { spec_id },
        _ => finish::FinishScope::All {
            project_id: request.project_id,
        },
    };
    if let finish::FinishScope::One { spec_id } = &scope
        && !matches!(store::get_spec(&state.pool, spec_id).await, Ok(Some(_)))
    {
        return StatusCode::ACCEPTED;
    }
    let state_for_task = state.clone();
    tokio::spawn(async move {
        if let Err(error) = finish::finish(&state_for_task, scope).await {
            match error {
                finish::FinishError::Store(error) => tracing::warn!(%error, "backlog finish store failed"),
                finish::FinishError::Config(error) => tracing::warn!(%error, "backlog finish configuration failed"),
                other => tracing::warn!(?other, "backlog finish failed"),
            }
        }
    });
    StatusCode::ACCEPTED
}

async fn get_decisions(
    State(state): State<AppState>,
) -> Result<Json<Vec<crate::spec_backlog::model::OwnerDecision>>, StatusCode> {
    store::list_owner_decisions(&state.pool)
        .await
        .map(Json)
        .map_err(|error| {
            tracing::warn!(%error, "could not list owner decisions");
            StatusCode::INTERNAL_SERVER_ERROR
        })
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
