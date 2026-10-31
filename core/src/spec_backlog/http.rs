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
                finish::FinishError::Store(error) => {
                    tracing::warn!(%error, "backlog finish store failed")
                }
                finish::FinishError::Config(error) => {
                    tracing::warn!(%error, "backlog finish configuration failed")
                }
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

    use axum::Router;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
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
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
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

        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
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

    #[tokio::test]
    async fn scan_reads_the_projects_real_specs() {
        let state = state().await;
        let root = tempfile::tempdir().unwrap();
        let specs = root.path().join(".ai/specs");
        std::fs::create_dir_all(&specs).unwrap();
        std::fs::write(specs.join("a.md"), "# A\n").unwrap();
        sqlx::query("INSERT INTO autopilot_state (project_id, mode, project_root) VALUES (?, 'off', ?)")
            .bind("scan-real-project")
            .bind(root.path().to_string_lossy().to_string())
            .execute(&state.pool).await.unwrap();
        let app = app(&state);

        let (status, body) = request(&app, "POST", "/specs/scan", json!({"project_id": "scan-real-project"})).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, json!({"new": 1, "changed": 0, "unchanged": 0}));
        let (status, specs) = request(&app, "GET", "/specs", Value::Null).await;
        assert_eq!(status, StatusCode::OK);
        assert!(specs.as_array().unwrap().iter().any(|spec| spec["path"] == ".ai/specs/a.md"));
    }

    #[tokio::test]
    async fn scan_of_a_project_without_a_root_is_refused() {
        let state = state().await;
        sqlx::query("INSERT INTO autopilot_state (project_id, mode, project_root) VALUES ('scan-no-root', 'off', NULL)")
            .execute(&state.pool).await.unwrap();
        let (status, body) = request(&app(&state), "POST", "/specs/scan", json!({"project_id": "scan-no-root"})).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(!body["error"].as_str().unwrap_or_default().is_empty());
    }

    #[tokio::test]
    async fn finish_without_a_project_is_refused() {
        let state = state().await;
        let (status, body) = request(&app(&state), "POST", "/backlog/finish", json!({})).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(!body["error"].as_str().unwrap_or_default().is_empty());
    }

    #[tokio::test]
    async fn finish_refuses_what_it_cannot_do() {
        let state = state().await;
        let app = app(&state);
        let (status, _) = request(&app, "POST", "/backlog/finish", json!({"spec_id": "nope"})).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        let (status, _) = request(&app, "POST", "/backlog/finish", json!({"project_id": "no-root"})).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[tokio::test]
    async fn a_second_finish_for_a_project_is_refused_while_one_runs() {
        let state = state().await;
        let root = tempfile::tempdir().unwrap();
        sqlx::query("INSERT INTO autopilot_state (project_id, mode, project_root) VALUES (?, 'off', ?)")
            .bind("finish-claim-project")
            .bind(root.path().to_string_lossy().to_string())
            .execute(&state.pool).await.unwrap();
        let claim = crate::spec_backlog::finish::claim_project("finish-claim-project").expect("first claim");
        let (status, _) = request(&app(&state), "POST", "/backlog/finish", json!({"project_id": "finish-claim-project"})).await;
        assert_eq!(status, StatusCode::CONFLICT);
        drop(claim);
        let (status, _) = request(&app(&state), "POST", "/backlog/finish", json!({"project_id": "finish-claim-project"})).await;
        assert_eq!(status, StatusCode::ACCEPTED);
    }

    #[tokio::test]
    async fn finish_finds_the_project_by_its_root() {
        let state = state().await;
        let root = tempfile::tempdir().unwrap();
        let path = root.path().to_string_lossy().to_string();
        sqlx::query("INSERT INTO autopilot_state (project_id, mode, project_root) VALUES ('finish-root-project', 'off', ?)")
            .bind(&path).execute(&state.pool).await.unwrap();
        let app = app(&state);
        for root in [path.replace('\\', "/"), path.replace('/', "\\")] {
            let (status, _) = request(&app, "POST", "/backlog/finish", json!({"project_root": root})).await;
            assert_eq!(status, StatusCode::ACCEPTED);
        }
        let (status, _) = request(&app, "POST", "/backlog/finish", json!({"project_root": "C:/nowhere"})).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[tokio::test]
    async fn extract_and_audit_answer_for_what_they_can_do() {
        let state = state().await;
        sqlx::query("INSERT INTO autopilot_state (project_id, mode, project_root) VALUES ('rootless-spec-project', 'off', NULL)")
            .execute(&state.pool).await.unwrap();
        sqlx::query(
            "INSERT INTO specs (id, project_id, path, title, content_sha, created_at, updated_at) \
             VALUES ('rootless-spec', 'rootless-spec-project', '.ai/specs/a.md', 'A', 'sha', 'now', 'now')",
        ).execute(&state.pool).await.unwrap();
        let app = app(&state);
        for route in ["/specs/nope/extract", "/specs/nope/audit"] {
            let (status, _) = request(&app, "POST", route, json!({})).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{route}");
        }
        for route in ["/specs/rootless-spec/extract", "/specs/rootless-spec/audit"] {
            let (status, _) = request(&app, "POST", route, json!({})).await;
            assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{route}");
        }
    }

    #[tokio::test]
    async fn the_owner_can_add_a_dependency() {
        let state = state().await;
        sqlx::query("INSERT INTO specs (id, path, title, content_sha, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?)")
            .bind("dep-a").bind(".ai/specs/a.md").bind("A").bind("a").bind("now").bind("now")
            .execute(&state.pool).await.unwrap();
        sqlx::query("INSERT INTO specs (id, path, title, content_sha, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?)")
            .bind("dep-b").bind(".ai/specs/b.md").bind("B").bind("b").bind("now").bind("now")
            .execute(&state.pool).await.unwrap();
        let app = app(&state);
        let (status, _) = request(&app, "POST", "/specs/dep-a/deps", json!({"depends_on": "dep-b"})).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(crate::spec_backlog::store::list_deps(&state.pool, "dep-a").await.unwrap()[0].source, crate::spec_backlog::model::DepSource::Owner);
        for (route, body, expected) in [
            ("/specs/nope/deps", json!({"depends_on": "dep-b"}), StatusCode::NOT_FOUND),
            ("/specs/dep-a/deps", json!({"depends_on": "nope"}), StatusCode::UNPROCESSABLE_ENTITY),
            ("/specs/dep-a/deps", json!({"depends_on": "dep-a"}), StatusCode::UNPROCESSABLE_ENTITY),
        ] {
            let (status, _) = request(&app, "POST", route, body).await;
            assert_eq!(status, expected, "{route}");
        }
    }
}
