//! HTTP routes for the spec backlog.
//!
//! A route that starts slow work (a finish, an extraction, an audit) checks what it can before it
//! answers: a refusal is a 4xx with `{"error": ..}`, and only a request that can run is a 202.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::spec_backlog::model::DepSource;
use crate::spec_backlog::{finish, scan, store};
use crate::state::AppState;

#[derive(Debug, Deserialize)]
struct FinishRequest {
    spec_id: Option<String>,
    project_id: Option<String>,
    project_root: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ScanRequest {
    project_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DepRequest {
    depends_on: String,
}

type Answer = (StatusCode, Json<Value>);

fn refuse(status: StatusCode, error: impl std::fmt::Display) -> Answer {
    (status, Json(json!({ "error": error.to_string() })))
}

fn failed(what: &str, error: impl std::fmt::Display) -> Answer {
    tracing::warn!(%error, "{what}");
    refuse(
        StatusCode::INTERNAL_SERVER_ERROR,
        format!("{what}: {error}"),
    )
}

fn non_empty(value: Option<String>) -> Option<String> {
    value.filter(|value| !value.trim().is_empty())
}

/// Routes supplied to the daemon's protected router.
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/specs/scan", post(post_scan))
        .route("/specs", get(get_specs))
        .route("/specs/{id}/extract", post(post_extract))
        .route("/specs/{id}/audit", post(post_audit))
        .route("/specs/{id}/deps", post(post_deps))
        .route("/backlog", get(get_backlog))
        .route("/backlog/finish", post(post_finish))
        .route("/decisions", get(get_decisions))
}

/// Reads the project's real spec directories into the backlog.
async fn post_scan(State(state): State<AppState>, Json(request): Json<ScanRequest>) -> Answer {
    let Some(project_id) = non_empty(request.project_id) else {
        return refuse(StatusCode::UNPROCESSABLE_ENTITY, "project_id is required");
    };
    let paths = match finish::resolve_project(&state, &project_id).await {
        Ok(paths) => paths,
        Err(error) => return refuse(StatusCode::UNPROCESSABLE_ENTITY, error),
    };
    let roots = scan::ScanRoots {
        project_root: Some(paths.project_root.into()),
        state_dir: paths.state_dir,
    };
    match scan::scan(&state.pool, Some(&project_id), &roots).await {
        Ok(report) => (
            StatusCode::OK,
            Json(json!({
                "new": report.new,
                "changed": report.changed,
                "unchanged": report.unchanged,
            })),
        ),
        Err(error) => failed("spec scan failed", format!("{error:?}")),
    }
}

async fn get_specs(
    State(state): State<AppState>,
) -> Result<Json<Vec<crate::spec_backlog::model::Spec>>, StatusCode> {
    store::list_all_specs(&state.pool)
        .await
        .map(Json)
        .map_err(|error| {
            tracing::warn!(%error, "could not list specs");
            StatusCode::INTERNAL_SERVER_ERROR
        })
}

/// What `extract` and `audit` check before they accept: the spec exists, and its project has a
/// root to read it from.
async fn check_spec_can_run(state: &AppState, spec_id: &str) -> Result<(), Answer> {
    let spec = match store::get_spec(&state.pool, spec_id).await {
        Ok(Some(spec)) => spec,
        Ok(None) => {
            return Err(refuse(
                StatusCode::NOT_FOUND,
                format!("spec {spec_id} not found"),
            ));
        }
        Err(error) => return Err(failed("could not read the spec", error)),
    };
    let Some(project_id) = spec.project_id else {
        return Err(refuse(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("spec {spec_id} has no project"),
        ));
    };
    finish::resolve_project(state, &project_id)
        .await
        .map(|_| ())
        .map_err(|error| refuse(StatusCode::UNPROCESSABLE_ENTITY, error))
}

/// Extraction runs a paid agent, so the route only waits for acceptance of the work.
async fn post_extract(State(state): State<AppState>, Path(spec_id): Path<String>) -> Answer {
    if let Err(answer) = check_spec_can_run(&state, &spec_id).await {
        return answer;
    }
    tokio::spawn(async move {
        if let Err(error) = finish::extract_one(&state, &spec_id).await {
            tracing::warn!(%error, %spec_id, "spec extraction failed");
        }
    });
    (StatusCode::ACCEPTED, Json(json!({ "accepted": true })))
}

/// An audit runs a reviewer, so the route only waits for acceptance of the work.
async fn post_audit(State(state): State<AppState>, Path(spec_id): Path<String>) -> Answer {
    if let Err(answer) = check_spec_can_run(&state, &spec_id).await {
        return answer;
    }
    tokio::spawn(async move {
        if let Err(error) = finish::audit_one(&state, &spec_id).await {
            tracing::warn!(%error, %spec_id, "spec audit failed");
        }
    });
    (StatusCode::ACCEPTED, Json(json!({ "accepted": true })))
}

/// The owner says that `id` waits on another spec. A cycle is accepted on purpose: `finish` sends
/// it to the owner as a question.
async fn post_deps(
    State(state): State<AppState>,
    Path(spec_id): Path<String>,
    Json(request): Json<DepRequest>,
) -> Answer {
    match store::get_spec(&state.pool, &spec_id).await {
        Ok(Some(_)) => {}
        Ok(None) => {
            return refuse(StatusCode::NOT_FOUND, format!("spec {spec_id} not found"));
        }
        Err(error) => return failed("could not read the spec", error),
    }
    if request.depends_on == spec_id {
        return refuse(
            StatusCode::UNPROCESSABLE_ENTITY,
            "a spec cannot depend on itself",
        );
    }
    match store::get_spec(&state.pool, &request.depends_on).await {
        Ok(Some(_)) => {}
        Ok(None) => {
            return refuse(
                StatusCode::UNPROCESSABLE_ENTITY,
                format!("spec {} not found", request.depends_on),
            );
        }
        Err(error) => return failed("could not read the spec", error),
    }
    match store::add_dep(&state.pool, &spec_id, &request.depends_on, DepSource::Owner).await {
        Ok(()) => (
            StatusCode::OK,
            Json(json!({ "spec_id": spec_id, "depends_on": request.depends_on })),
        ),
        Err(error) => failed("could not record the dependency", error),
    }
}

async fn get_backlog(
    State(state): State<AppState>,
) -> Result<Json<Vec<crate::spec_backlog::model::Spec>>, StatusCode> {
    get_specs(State(state)).await
}

/// Starts a finish when it can run: the scope names a known spec or a project with a root, and no
/// other finish holds that project. The claim lives as long as the finish does.
async fn post_finish(State(state): State<AppState>, Json(request): Json<FinishRequest>) -> Answer {
    let (scope, project_id) = if let Some(spec_id) = non_empty(request.spec_id) {
        match store::get_spec(&state.pool, &spec_id).await {
            Ok(Some(spec)) => match spec.project_id {
                Some(project_id) => (finish::FinishScope::One { spec_id }, project_id),
                None => {
                    return refuse(
                        StatusCode::UNPROCESSABLE_ENTITY,
                        format!("spec {spec_id} has no project"),
                    );
                }
            },
            Ok(None) => {
                return refuse(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    format!("spec {spec_id} not found"),
                );
            }
            Err(error) => return failed("could not read the spec", error),
        }
    } else if let Some(project_id) = non_empty(request.project_id) {
        (
            finish::FinishScope::All {
                project_id: Some(project_id.clone()),
            },
            project_id,
        )
    } else if let Some(root) = non_empty(request.project_root) {
        match finish::project_id_for_root(&state.pool, &root).await {
            Ok(Some(project_id)) => (
                finish::FinishScope::All {
                    project_id: Some(project_id.clone()),
                },
                project_id,
            ),
            Ok(None) => {
                return refuse(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    format!("no project has the root {root}"),
                );
            }
            Err(error) => return failed("could not look the project up", error),
        }
    } else {
        return refuse(
            StatusCode::UNPROCESSABLE_ENTITY,
            "name a spec_id, a project_id or a project_root",
        );
    };
    if let Err(error) = finish::resolve_project(&state, &project_id).await {
        return refuse(StatusCode::UNPROCESSABLE_ENTITY, error);
    }
    let Some(claim) = finish::claim_project(&project_id) else {
        return refuse(
            StatusCode::CONFLICT,
            format!("a finish is already running for project {project_id}"),
        );
    };
    tokio::spawn(async move {
        let _claim = claim;
        if let Err(error) = finish::finish(&state, scope).await {
            match &error {
                finish::FinishError::Store(error) => {
                    tracing::warn!(%error, "backlog finish store failed")
                }
                finish::FinishError::Config(error) => {
                    tracing::warn!(%error, "backlog finish configuration failed")
                }
                finish::FinishError::Scan(error) => {
                    tracing::warn!(%error, "backlog finish scan failed")
                }
            }
            // The owner is not watching this task: the feed is where a failed finish shows.
            finish::announce_failure(&state.pool, &project_id, &error.to_string()).await;
        }
    });
    (StatusCode::ACCEPTED, Json(json!({ "accepted": true })))
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

    /// A spec that is not waiting on the owner, so answering never reopens it and no run starts.
    async fn seed_question(state: &AppState, id: &str, status: &str) {
        sqlx::query(
            "INSERT OR IGNORE INTO specs (id, path, title, content_sha, created_at, updated_at) \
             VALUES ('spec-1', 'C:/spec.md', 'Spec', 'sha', '2026-10-30T00:00:00Z', '2026-10-30T00:00:00Z')",
        )
        .execute(&state.pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO spec_questions (id, spec_id, question, status, created_at) \
             VALUES (?, 'spec-1', 'Which budget?', ?, '2026-10-30T00:00:00Z')",
        )
        .bind(id)
        .bind(status)
        .execute(&state.pool)
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn post_decision_answers_an_sq_question() {
        let state = state().await;
        seed_question(&state, "sq-http-1", "open").await;

        let (status, _) = request(
            &app(&state),
            "POST",
            "/decisions/sq-http-1",
            json!({"answer": "ten dollars"}),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        let (stored, answer): (String, Option<String>) =
            sqlx::query_as("SELECT status, answer FROM spec_questions WHERE id = 'sq-http-1'")
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(stored, "answered");
        assert_eq!(answer.as_deref(), Some("ten dollars"));
    }

    #[tokio::test]
    async fn post_decision_refuses_an_answered_question_with_409() {
        let state = state().await;
        seed_question(&state, "sq-http-2", "open").await;
        let app = app(&state);
        let (status, _) = request(
            &app,
            "POST",
            "/decisions/sq-http-2",
            json!({"answer": "first"}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        let (status, _) = request(
            &app,
            "POST",
            "/decisions/sq-http-2",
            json!({"answer": "second"}),
        )
        .await;

        assert_eq!(status, StatusCode::CONFLICT);
        let answer: Option<String> =
            sqlx::query_scalar("SELECT answer FROM spec_questions WHERE id = 'sq-http-2'")
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(answer.as_deref(), Some("first"));
    }

    #[tokio::test]
    async fn post_decision_unknown_id_is_404() {
        let state = state().await;

        let (status, _) = request(
            &app(&state),
            "POST",
            "/decisions/sq-nope",
            json!({"answer": "anything"}),
        )
        .await;

        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn scan_reads_the_projects_real_specs() {
        let state = state().await;
        let root = tempfile::tempdir().unwrap();
        let specs = root.path().join(".ai/specs");
        std::fs::create_dir_all(&specs).unwrap();
        std::fs::write(specs.join("a.md"), "# A\n").unwrap();
        sqlx::query(
            "INSERT INTO autopilot_state (project_id, mode, project_root) VALUES (?, 'off', ?)",
        )
        .bind("scan-real-project")
        .bind(root.path().to_string_lossy().to_string())
        .execute(&state.pool)
        .await
        .unwrap();
        let app = app(&state);

        let (status, body) = request(
            &app,
            "POST",
            "/specs/scan",
            json!({"project_id": "scan-real-project"}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, json!({"new": 1, "changed": 0, "unchanged": 0}));
        let (status, specs) = request(&app, "GET", "/specs", Value::Null).await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            specs
                .as_array()
                .unwrap()
                .iter()
                .any(|spec| spec["path"] == ".ai/specs/a.md")
        );
    }

    #[tokio::test]
    async fn scan_of_a_project_without_a_root_is_refused() {
        let state = state().await;
        sqlx::query("INSERT INTO autopilot_state (project_id, mode, project_root) VALUES ('scan-no-root', 'off', NULL)")
            .execute(&state.pool).await.unwrap();
        let (status, body) = request(
            &app(&state),
            "POST",
            "/specs/scan",
            json!({"project_id": "scan-no-root"}),
        )
        .await;
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
        let (status, _) =
            request(&app, "POST", "/backlog/finish", json!({"spec_id": "nope"})).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        let (status, _) = request(
            &app,
            "POST",
            "/backlog/finish",
            json!({"project_id": "no-root"}),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[tokio::test]
    async fn a_second_finish_for_a_project_is_refused_while_one_runs() {
        let state = state().await;
        let root = tempfile::tempdir().unwrap();
        sqlx::query(
            "INSERT INTO autopilot_state (project_id, mode, project_root) VALUES (?, 'off', ?)",
        )
        .bind("finish-claim-project")
        .bind(root.path().to_string_lossy().to_string())
        .execute(&state.pool)
        .await
        .unwrap();
        let claim = crate::spec_backlog::finish::claim_project("finish-claim-project")
            .expect("first claim");
        let (status, _) = request(
            &app(&state),
            "POST",
            "/backlog/finish",
            json!({"project_id": "finish-claim-project"}),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        drop(claim);
        let (status, _) = request(
            &app(&state),
            "POST",
            "/backlog/finish",
            json!({"project_id": "finish-claim-project"}),
        )
        .await;
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
        for (index, root) in [path.replace('\\', "/"), path.replace('/', "\\")]
            .into_iter()
            .enumerate()
        {
            let (status, _) = request(
                &app,
                "POST",
                "/backlog/finish",
                json!({"project_root": root}),
            )
            .await;
            if index == 0 {
                assert_eq!(status, StatusCode::ACCEPTED);
            } else {
                assert!(matches!(
                    status,
                    StatusCode::ACCEPTED | StatusCode::CONFLICT
                ));
            }
        }
        let (status, _) = request(
            &app,
            "POST",
            "/backlog/finish",
            json!({"project_root": "C:/nowhere"}),
        )
        .await;
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
        let (status, _) = request(
            &app,
            "POST",
            "/specs/dep-a/deps",
            json!({"depends_on": "dep-b"}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            crate::spec_backlog::store::list_deps(&state.pool, "dep-a")
                .await
                .unwrap()[0]
                .source,
            crate::spec_backlog::model::DepSource::Owner
        );
        for (route, body, expected) in [
            (
                "/specs/nope/deps",
                json!({"depends_on": "dep-b"}),
                StatusCode::NOT_FOUND,
            ),
            (
                "/specs/dep-a/deps",
                json!({"depends_on": "nope"}),
                StatusCode::UNPROCESSABLE_ENTITY,
            ),
            (
                "/specs/dep-a/deps",
                json!({"depends_on": "dep-a"}),
                StatusCode::UNPROCESSABLE_ENTITY,
            ),
        ] {
            let (status, _) = request(&app, "POST", route, body).await;
            assert_eq!(status, expected, "{route}");
        }
    }
}
