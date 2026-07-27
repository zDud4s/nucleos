use axum::Json;
use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use serde::Deserialize;
use std::path::PathBuf;
use tower_http::cors::{Any, CorsLayer};

use crate::auth::require_token;
use crate::autopilot::{self, ActivationError, Mode, ProjectSummary, ScopedKill};
use crate::budget;
use crate::feed::{self, FeedEntry};
use crate::hooks::pretooluse_decision;
use crate::inspect;
use crate::runs::{self, AwaitingRun, CreateRunError, cancel_run, create_run, get_run};
use crate::shadow::{self, ClassTally, ShadowDecision};
use crate::state::AppState;
use crate::worktree::{self, ReleaseOutcome};

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
        .route(
            "/autopilot/kill",
            get(get_autopilot_kill).post(post_autopilot_kill),
        )
        .route(
            "/autopilot/kill/scoped",
            get(get_autopilot_kill_scoped).post(post_autopilot_kill_scoped),
        )
        .route(
            "/autopilot/budget",
            get(get_autopilot_budget).post(post_autopilot_budget),
        )
        .route("/projects", get(get_projects))
        .route("/projects/{id}/ls", get(get_project_ls))
        .route("/projects/{id}/cat", get(get_project_cat))
        .route("/projects/{id}/grep", get(get_project_grep))
        .route("/projects/{id}/diff", get(get_project_diff))
        .route("/feed", get(get_feed))
        .route("/runs", post(create_run))
        // The literal path coexists with `/runs/{id}`; static segments win in matchit.
        .route("/runs/awaiting-approval", get(list_awaiting_approval_runs))
        .route("/runs/{id}", get(get_run))
        .route("/runs/{id}/cancel", post(cancel_run))
        .route("/assistant/message", post(post_assistant_message))
        .route("/assistant/{turn_id}", get(get_run))
        .route("/proposals", get(get_proposals))
        .route("/proposals/{id}/approve", post(post_proposal_approve))
        .route("/proposals/{id}/reject", post(post_proposal_reject))
        .route("/worktrees/{run_id}/release", post(post_worktree_release))
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
    scope: Option<String>,
}

#[derive(Deserialize)]
struct PathQuery {
    path: Option<String>,
}

#[derive(Deserialize)]
struct GrepQuery {
    q: Option<String>,
    path: Option<String>,
}

#[derive(Deserialize)]
struct AssistantMessageRequest {
    chat_id: String,
    text: String,
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

#[derive(Deserialize)]
struct ScopedKillRequest {
    scope_type: String,
    scope_id: String,
    engaged: bool,
}

#[derive(serde::Serialize)]
struct AutopilotKillResponse {
    engaged: bool,
}

#[derive(serde::Serialize)]
struct BudgetResponse {
    limit_usd: Option<f64>,
    period: String,
    hourly_limit_usd: Option<f64>,
    per_run_reserve_usd: f64,
    time_cost_per_hour_usd: f64,
    window_spend_usd: f64,
    hourly_spend_usd: f64,
    paused: bool,
    reason: Option<String>,
}

#[derive(Deserialize)]
struct BudgetRequest {
    limit_usd: Option<f64>,
    period: String,
    hourly_limit_usd: Option<f64>,
    per_run_reserve_usd: f64,
    time_cost_per_hour_usd: f64,
}

async fn post_assistant_message(
    State(state): State<AppState>,
    Json(body): Json<AssistantMessageRequest>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    match crate::assistant::send_message(&state, &body.chat_id, &body.text).await {
        Ok(turn_id) => Ok(Json(serde_json::json!({ "turn_id": turn_id }))),
        Err(msg) if msg.contains("already in progress") => Err(StatusCode::CONFLICT),
        Err(_) => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
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

async fn get_autopilot_kill(
    State(state): State<AppState>,
) -> Result<Json<AutopilotKillResponse>, StatusCode> {
    let engaged = autopilot::kill_switch_engaged(&state.pool)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(AutopilotKillResponse { engaged }))
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

async fn get_autopilot_kill_scoped(
    State(state): State<AppState>,
) -> Result<Json<Vec<ScopedKill>>, StatusCode> {
    autopilot::list_scoped_kills(&state.pool)
        .await
        .map(Json)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

async fn post_autopilot_kill_scoped(
    State(state): State<AppState>,
    Json(body): Json<ScopedKillRequest>,
) -> Result<StatusCode, StatusCode> {
    if body.scope_type != "project" && body.scope_type != "trigger" {
        return Err(StatusCode::BAD_REQUEST);
    }
    autopilot::set_scoped_kill(&state.pool, &body.scope_type, &body.scope_id, body.engaged)
        .await
        .map(|()| StatusCode::NO_CONTENT)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

async fn get_autopilot_budget(
    State(state): State<AppState>,
) -> Result<Json<BudgetResponse>, StatusCode> {
    budget_response(&state).await.map(Json)
}

async fn post_autopilot_budget(
    State(state): State<AppState>,
    Json(body): Json<BudgetRequest>,
) -> Result<Json<BudgetResponse>, StatusCode> {
    let period = budget::BudgetPeriod::from_db_str(&body.period).ok_or(StatusCode::BAD_REQUEST)?;
    let config = budget::BudgetConfig {
        limit_usd: body.limit_usd,
        period,
        hourly_limit_usd: body.hourly_limit_usd,
        per_run_reserve_usd: body.per_run_reserve_usd,
        time_cost_per_hour_usd: body.time_cost_per_hour_usd,
    };
    budget::set_budget_config(&state.pool, &config)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    budget_response(&state).await.map(Json)
}

async fn budget_response(state: &AppState) -> Result<BudgetResponse, StatusCode> {
    let now = chrono::Utc::now();
    let config = budget::load_budget_config(&state.pool)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let window_spend_usd = budget::window_spend(&state.pool, now)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let hourly_spend_usd = budget::hourly_spend(&state.pool, now)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let (paused, reason) = match budget::budget_permits_new_run(&state.pool, now).await {
        budget::BudgetDecision::Allow => (false, None),
        budget::BudgetDecision::Pause { reason } => (true, Some(reason)),
    };
    Ok(BudgetResponse {
        limit_usd: config.limit_usd,
        period: config.period.as_db_str().to_string(),
        hourly_limit_usd: config.hourly_limit_usd,
        per_run_reserve_usd: config.per_run_reserve_usd,
        time_cost_per_hour_usd: config.time_cost_per_hour_usd,
        window_spend_usd,
        hourly_spend_usd,
        paused,
        reason,
    })
}

fn activation_status(error: ActivationError) -> StatusCode {
    match error {
        ActivationError::NotAGitRepo
        | ActivationError::ProjectRootRequired
        | ActivationError::NotOnboarded
        | ActivationError::HookNotRegistered => StatusCode::UNPROCESSABLE_ENTITY,
        ActivationError::Database(_) => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

fn inspect_status(error: inspect::InspectError) -> StatusCode {
    match error {
        inspect::InspectError::NoRoot | inspect::InspectError::NotFound => StatusCode::NOT_FOUND,
        inspect::InspectError::UnsafePath => StatusCode::BAD_REQUEST,
        inspect::InspectError::Io(_) => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

async fn resolve_project_root(state: &AppState, id: &str) -> Result<PathBuf, StatusCode> {
    match inspect::project_root(&state.pool, id).await {
        Ok(Some(root)) => Ok(PathBuf::from(root)),
        Ok(None) => Err(StatusCode::NOT_FOUND),
        Err(_) => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

async fn get_projects(
    State(state): State<AppState>,
) -> Result<Json<Vec<ProjectSummary>>, StatusCode> {
    autopilot::project_roster(&state.pool)
        .await
        .map(Json)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

async fn get_project_ls(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<PathQuery>,
) -> Result<Json<Vec<inspect::Entry>>, StatusCode> {
    let root = resolve_project_root(&state, &id).await?;
    let rel = query.path.unwrap_or_default();
    tokio::task::spawn_blocking(move || inspect::ls(&root, &rel))
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .map(Json)
        .map_err(inspect_status)
}

async fn get_project_cat(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<PathQuery>,
) -> Result<String, StatusCode> {
    let root = resolve_project_root(&state, &id).await?;
    let rel = query.path.unwrap_or_default();
    tokio::task::spawn_blocking(move || inspect::cat(&root, &rel))
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .map_err(inspect_status)
}

async fn get_project_grep(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<GrepQuery>,
) -> Result<Json<Vec<inspect::Match>>, StatusCode> {
    let root = resolve_project_root(&state, &id).await?;
    let q = query.q.unwrap_or_default();
    let rel = query.path.unwrap_or_default();
    tokio::task::spawn_blocking(move || inspect::grep(&root, &q, &rel))
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .map(Json)
        .map_err(inspect_status)
}

async fn get_project_diff(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<String, StatusCode> {
    let root = resolve_project_root(&state, &id).await?;
    tokio::task::spawn_blocking(move || inspect::diff(&root))
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .map_err(inspect_status)
}

pub(crate) fn create_run_status(error: &CreateRunError) -> StatusCode {
    match error {
        CreateRunError::Invalid(_) => StatusCode::BAD_REQUEST,
        CreateRunError::Busy => StatusCode::CONFLICT,
        CreateRunError::Worktree(_) | CreateRunError::Db(_) => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

async fn get_feed(
    State(state): State<AppState>,
    Query(query): Query<FeedQuery>,
) -> Result<Json<Vec<FeedEntry>>, StatusCode> {
    let entries = if query.scope.as_deref() == Some("all") {
        feed::list_all(&state.pool, 50).await
    } else {
        feed::list_feed(&state.pool, query.project_id.as_deref(), 50).await
    };
    entries
        .map(Json)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

async fn list_awaiting_approval_runs(
    State(state): State<AppState>,
) -> Result<Json<Vec<AwaitingRun>>, StatusCode> {
    runs::list_awaiting_approval(&state.pool)
        .await
        .map(Json)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

async fn get_proposals(
    State(state): State<AppState>,
) -> Result<Json<Vec<crate::proposals::Proposal>>, StatusCode> {
    crate::proposals::list_pending(&state.pool)
        .await
        .map(Json)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

async fn post_proposal_approve(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    match crate::runs::resume_approved_run(&state, id).await {
        Ok(resume_id) => Ok(Json(serde_json::json!({ "resume_run_id": resume_id }))),
        Err(crate::runs::ResumeError::ProposalNotFound) => Err(StatusCode::NOT_FOUND),
        Err(crate::runs::ResumeError::ProposalNotPending)
        | Err(crate::runs::ResumeError::NotResumable(_)) => Err(StatusCode::CONFLICT),
        Err(crate::runs::ResumeError::Db(_)) => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

async fn post_proposal_reject(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<StatusCode, StatusCode> {
    match crate::proposals::reject_proposal(&state.pool, id).await {
        Ok(()) => Ok(StatusCode::NO_CONTENT),
        Err(crate::proposals::RejectError::NotFound) => Err(StatusCode::NOT_FOUND),
        Err(crate::proposals::RejectError::NotPending) => Err(StatusCode::CONFLICT),
        Err(crate::proposals::RejectError::Db(_)) => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

async fn post_worktree_release(
    State(state): State<AppState>,
    Path(run_id): Path<i64>,
) -> Result<StatusCode, StatusCode> {
    match worktree::release(&state.pool, run_id).await {
        Ok(ReleaseOutcome::Released) => Ok(StatusCode::NO_CONTENT),
        Ok(ReleaseOutcome::NotAwaitingApproval) => Err(StatusCode::CONFLICT),
        Ok(ReleaseOutcome::NotFound) => Err(StatusCode::NOT_FOUND),
        Err(_) => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
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

    // Readiness only ever moves when a human reviews a decision, so this is the one place a project
    // can cross the promotion bar — sample it either side of the verdict to catch the crossing.
    let project = shadow::project_of_decision(&state.pool, id)
        .await
        .unwrap_or(None);
    let was_promotable = match &project {
        Some(project_id) => shadow::project_readiness(&state.pool, project_id)
            .await
            .map(|(ready, total)| shadow::promotable(ready, total))
            .unwrap_or(false),
        None => false,
    };

    match shadow::set_verdict(&state.pool, id, &body.verdict).await {
        Ok(true) => {
            if let Some(project_id) = project {
                announce_promotable(&state.pool, &project_id, was_promotable).await;
            }
            Ok(StatusCode::NO_CONTENT)
        }
        Ok(false) => Err(StatusCode::NOT_FOUND),
        Err(_) => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

/// The §8.2 promotion nudge: a feed entry the moment a project's last outstanding action class
/// clears the bar. Without it the gate solves promotion-by-impatience but leaves the opposite
/// failure — a project that quietly became promotable and nobody noticed.
///
/// Best-effort and idempotent by construction: it fires only on the false→true crossing, so a
/// project already promotable before the verdict stays silent. Feed failures never fail the verdict.
async fn announce_promotable(pool: &sqlx::SqlitePool, project_id: &str, was_promotable: bool) {
    if was_promotable {
        return;
    }
    let Ok((ready, total)) = shadow::project_readiness(pool, project_id).await else {
        return;
    };
    if !shadow::promotable(ready, total) {
        return;
    }

    let summary = format!(
        "{project_id} is ready for promotion — all {total} reviewed action classes clear the bar \
         ({}+ reviews, {}%+ agreement). Promotion is still yours to make.",
        shadow::READINESS_MIN_REVIEWED,
        shadow::READINESS_MIN_AGREE_PERCENT,
    );
    let _ = feed::append(pool, Some(project_id), "promotion_ready", &summary, None).await;
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
    use crate::proposals;
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
        sqlx::migrate!().run(&pool).await.unwrap();
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

    #[tokio::test]
    async fn projects_returns_json_array_with_bearer_token() {
        let app = build_router(test_state().await);
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/projects")
                    .header("Authorization", "Bearer test-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(parsed.is_array());
    }

    #[tokio::test]
    async fn projects_rejects_requests_without_bearer_token() {
        let app = build_router(test_state().await);
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/projects")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn project_ls_returns_404_when_project_has_no_root() {
        let app = build_router(test_state().await);
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/projects/ghost/ls")
                    .header("Authorization", "Bearer test-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn project_cat_rejects_unsafe_path() {
        let state = test_state().await;
        let root = tempfile::tempdir().unwrap();
        sqlx::query(
            "INSERT INTO autopilot_state (project_id, mode, project_root) VALUES ('p', 'active', ?)",
        )
        .bind(root.path().to_string_lossy().into_owned())
        .execute(&state.pool)
        .await
        .unwrap();
        let app = build_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/projects/p/cat?path=..%2f..%2fsecret")
                    .header("Authorization", "Bearer test-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn assistant_message_returns_turn_id_with_bearer_token() {
        let app = build_router(test_state().await);
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/assistant/message")
                    .header("Authorization", "Bearer test-token")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&serde_json::json!({
                            "chat_id": "chat-1",
                            "text": "hello"
                        }))
                        .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(parsed["turn_id"].is_number());
    }

    #[tokio::test]
    async fn assistant_message_rejects_without_bearer_token() {
        let app = build_router(test_state().await);
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/assistant/message")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&serde_json::json!({
                            "chat_id": "chat-1",
                            "text": "hello"
                        }))
                        .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn assistant_turn_status_returns_run_after_message() {
        let app = build_router(test_state().await);
        let post_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/assistant/message")
                    .header("Authorization", "Bearer test-token")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&serde_json::json!({
                            "chat_id": "chat-2",
                            "text": "hello"
                        }))
                        .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(post_response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(post_response.into_body(), usize::MAX)
            .await
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let turn_id = parsed["turn_id"].as_i64().unwrap();

        let get_response = app
            .oneshot(
                Request::builder()
                    .uri(format!("/assistant/{turn_id}"))
                    .header("Authorization", "Bearer test-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(get_response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(get_response.into_body(), usize::MAX)
            .await
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(parsed["id"], serde_json::json!(turn_id));
    }

    #[tokio::test]
    async fn awaiting_approval_runs_returns_seeded_run_with_bearer_token() {
        let state = test_state().await;
        let pool = state.pool.clone();
        sqlx::query(
            "INSERT INTO runs (project_id, cwd, prompt, status, mode, created_at)
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind("project-alpha")
        .bind("C:/worktrees/project-alpha/run-1")
        .bind("release the pinned worktree")
        .bind("awaiting_approval")
        .bind("worktree")
        .bind("2026-07-20T10:11:12Z")
        .execute(&pool)
        .await
        .unwrap();
        let app = build_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/runs/awaiting-approval")
                    .header("Authorization", "Bearer test-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            parsed,
            serde_json::json!([{
                "id": 1,
                "project_id": "project-alpha",
                "prompt": "release the pinned worktree",
                "cwd": "C:/worktrees/project-alpha/run-1",
                "created_at": "2026-07-20T10:11:12Z"
            }])
        );
    }

    #[tokio::test]
    async fn awaiting_approval_runs_rejects_requests_without_bearer_token() {
        let app = build_router(test_state().await);
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/runs/awaiting-approval")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn autopilot_kill_get_returns_false_by_default() {
        let app = build_router(test_state().await);
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/autopilot/kill")
                    .header("Authorization", "Bearer test-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(parsed, serde_json::json!({ "engaged": false }));
    }

    #[tokio::test]
    async fn autopilot_kill_get_returns_true_after_post_engages_switch() {
        let app = build_router(test_state().await);
        let post_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/autopilot/kill")
                    .header("Authorization", "Bearer test-token")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&serde_json::json!({ "engaged": true })).unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(post_response.status(), StatusCode::NO_CONTENT);

        let get_response = app
            .oneshot(
                Request::builder()
                    .uri("/autopilot/kill")
                    .header("Authorization", "Bearer test-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(get_response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(get_response.into_body(), usize::MAX)
            .await
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(parsed, serde_json::json!({ "engaged": true }));
    }

    #[tokio::test]
    async fn autopilot_kill_get_rejects_requests_without_bearer_token() {
        let app = build_router(test_state().await);
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/autopilot/kill")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn feed_scope_all_returns_global_and_project_rows() {
        let state = test_state().await;
        let pool = state.pool.clone();
        crate::feed::append(&pool, None, "global", "global summary", None)
            .await
            .unwrap();
        crate::feed::append(
            &pool,
            Some("project-a"),
            "project",
            "project a summary",
            None,
        )
        .await
        .unwrap();
        let app = build_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/feed?scope=all")
                    .header("Authorization", "Bearer test-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let entries = parsed.as_array().unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0]["summary"], "project a summary");
        assert_eq!(entries[1]["summary"], "global summary");
    }

    #[tokio::test]
    async fn feed_project_query_returns_only_that_project() {
        let state = test_state().await;
        let pool = state.pool.clone();
        crate::feed::append(&pool, None, "global", "global summary", None)
            .await
            .unwrap();
        crate::feed::append(
            &pool,
            Some("project-a"),
            "project",
            "project a summary",
            None,
        )
        .await
        .unwrap();
        crate::feed::append(
            &pool,
            Some("project-b"),
            "project",
            "project b summary",
            None,
        )
        .await
        .unwrap();
        let app = build_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/feed?project_id=project-a")
                    .header("Authorization", "Bearer test-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let entries = parsed.as_array().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0]["project_id"], "project-a");
        assert_eq!(entries[0]["summary"], "project a summary");
    }

    #[tokio::test]
    async fn feed_without_query_returns_only_global_rows() {
        let state = test_state().await;
        let pool = state.pool.clone();
        crate::feed::append(&pool, None, "global", "global summary", None)
            .await
            .unwrap();
        crate::feed::append(
            &pool,
            Some("project-a"),
            "project",
            "project a summary",
            None,
        )
        .await
        .unwrap();
        let app = build_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/feed")
                    .header("Authorization", "Bearer test-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let entries = parsed.as_array().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0]["project_id"], serde_json::Value::Null);
        assert_eq!(entries[0]["summary"], "global summary");
    }

    #[tokio::test]
    async fn list_proposals_returns_pending_action_approvals() {
        let state = test_state().await;
        let pool = state.pool.clone();
        sqlx::query(
            "INSERT INTO runs (id, prompt, status, mode, created_at)
             VALUES (10, 'first run', 'awaiting_approval', 'worktree', '2026-07-20T12:00:00Z'),
                    (11, 'second run', 'awaiting_approval', 'worktree', '2026-07-20T12:01:00Z')",
        )
        .execute(&pool)
        .await
        .unwrap();
        proposals::create_action_approval(
            &pool,
            10,
            Some("s10"),
            Some("p"),
            "Bash",
            "first pending",
            None,
        )
        .await
        .unwrap();
        proposals::create_action_approval(
            &pool,
            11,
            Some("s11"),
            Some("p"),
            "Edit",
            "second pending",
            None,
        )
        .await
        .unwrap();
        let approved = proposals::create_action_approval(
            &pool,
            12,
            Some("s12"),
            Some("p"),
            "Write",
            "already approved",
            None,
        )
        .await
        .unwrap();
        assert!(
            proposals::transition(&pool, approved, "approved", "x")
                .await
                .unwrap()
        );
        let app = build_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/proposals")
                    .header("Authorization", "Bearer test-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let entries = parsed.as_array().unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0]["tool_name"], "Bash");
        assert_eq!(entries[0]["reasoning"], "first pending");
        assert_eq!(entries[0]["run_id"], 10);
        assert_eq!(entries[1]["tool_name"], "Edit");
        assert_eq!(entries[1]["reasoning"], "second pending");
        assert_eq!(entries[1]["run_id"], 11);
    }

    #[tokio::test]
    async fn list_proposals_rejects_without_token() {
        let app = build_router(test_state().await);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/proposals")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn reject_endpoint_discards_and_returns_204() {
        let state = test_state().await;
        let pool = state.pool.clone();
        let result = sqlx::query(
            "INSERT INTO runs (prompt, status, mode, created_at)
             VALUES ('reject this run', 'awaiting_approval', 'worktree', '2026-07-20T12:00:00Z')",
        )
        .execute(&pool)
        .await
        .unwrap();
        let run_id = result.last_insert_rowid();
        let proposal_id = proposals::create_action_approval(
            &pool,
            run_id,
            Some("s"),
            Some("p"),
            "Bash",
            "push needs approval",
            None,
        )
        .await
        .unwrap();
        let app = build_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/proposals/{proposal_id}/reject"))
                    .header("Authorization", "Bearer test-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        let proposal = proposals::get(&pool, proposal_id).await.unwrap().unwrap();
        assert_eq!(proposal.status, "rejected");
        let run_status = sqlx::query_scalar::<_, String>("SELECT status FROM runs WHERE id = ?")
            .bind(run_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(run_status, "cancelled");
    }

    #[tokio::test]
    async fn reject_unknown_proposal_returns_404() {
        let app = build_router(test_state().await);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/proposals/999999/reject")
                    .header("Authorization", "Bearer test-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn approve_endpoint_resumes_and_returns_the_resume_run_id() {
        let state = test_state().await;
        let pool = state.pool.clone();
        let created_at = chrono::Utc::now().to_rfc3339();
        let result = sqlx::query(
            "INSERT INTO runs (project_id, cwd, prompt, status, session_id, mode, created_at)
             VALUES ('proj', 'C:/worktrees/proj/run-paused', 'x', 'awaiting_approval',
                     'sess-a', 'worktree', ?)",
        )
        .bind(&created_at)
        .execute(&pool)
        .await
        .unwrap();
        let original_run_id = result.last_insert_rowid();
        sqlx::query(
            "INSERT INTO worktrees
             (run_id, project_id, project_root, path, branch, created_at)
             VALUES (?, 'proj', 'C:/repos/proj', 'C:/worktrees/proj/run-paused', ?, ?)",
        )
        .bind(original_run_id)
        .bind(format!("nucleos/run-{original_run_id}"))
        .bind(&created_at)
        .execute(&pool)
        .await
        .unwrap();
        let proposal_id = proposals::create_action_approval(
            &pool,
            original_run_id,
            Some("sess-a"),
            Some("proj"),
            "Bash",
            "push needs approval",
            Some("{}"),
        )
        .await
        .unwrap();
        let app = build_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/proposals/{proposal_id}/approve"))
                    .header("Authorization", "Bearer test-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(parsed["resume_run_id"].is_number());
        let proposal = proposals::get(&pool, proposal_id).await.unwrap().unwrap();
        assert_eq!(proposal.status, "approved");
    }

    #[tokio::test]
    async fn approve_unknown_proposal_returns_404() {
        let app = build_router(test_state().await);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/proposals/999999/approve")
                    .header("Authorization", "Bearer test-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn autopilot_budget_get_returns_defaults() {
        let app = build_router(test_state().await);
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/autopilot/budget")
                    .header("Authorization", "Bearer test-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            parsed,
            serde_json::json!({
                "limit_usd": null,
                "period": "monthly",
                "hourly_limit_usd": null,
                "per_run_reserve_usd": 0.5,
                "time_cost_per_hour_usd": 3.0,
                "window_spend_usd": 0.0,
                "hourly_spend_usd": 0.0,
                "paused": false,
                "reason": null
            })
        );
    }

    #[tokio::test]
    async fn autopilot_budget_post_sets_config_and_get_reflects_it() {
        let app = build_router(test_state().await);
        let post = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/autopilot/budget")
                    .header("Authorization", "Bearer test-token")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&serde_json::json!({
                            "limit_usd": 50.0,
                            "period": "weekly",
                            "hourly_limit_usd": 5.0,
                            "per_run_reserve_usd": 1.0,
                            "time_cost_per_hour_usd": 2.0
                        }))
                        .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(post.status(), StatusCode::OK);

        let get = app
            .oneshot(
                Request::builder()
                    .uri("/autopilot/budget")
                    .header("Authorization", "Bearer test-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(get.status(), StatusCode::OK);
        let body = axum::body::to_bytes(get.into_body(), usize::MAX)
            .await
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(parsed["limit_usd"], serde_json::json!(50.0));
        assert_eq!(parsed["period"], serde_json::json!("weekly"));
        assert_eq!(parsed["hourly_limit_usd"], serde_json::json!(5.0));
        assert_eq!(parsed["per_run_reserve_usd"], serde_json::json!(1.0));
        assert_eq!(parsed["time_cost_per_hour_usd"], serde_json::json!(2.0));
    }

    #[tokio::test]
    async fn autopilot_budget_post_rejects_invalid_period() {
        let app = build_router(test_state().await);
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/autopilot/budget")
                    .header("Authorization", "Bearer test-token")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&serde_json::json!({
                            "limit_usd": 10.0,
                            "period": "yearly",
                            "hourly_limit_usd": null,
                            "per_run_reserve_usd": 0.5,
                            "time_cost_per_hour_usd": 3.0
                        }))
                        .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn autopilot_budget_get_reports_paused_when_over_budget() {
        let state = test_state().await;
        let pool = state.pool.clone();
        let app = build_router(state);

        // $5 of autonomous spend recorded "now" (in the current window and hour).
        sqlx::query(
            "INSERT INTO runs (project_id, prompt, status, mode, cost_usd, created_at, completed_at)
             VALUES ('proj', 'prior spend', 'completed', 'worktree', 5.0, ?, ?)",
        )
        .bind(chrono::Utc::now().to_rfc3339())
        .bind(chrono::Utc::now().to_rfc3339())
        .execute(&pool)
        .await
        .unwrap();

        let post = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/autopilot/budget")
                    .header("Authorization", "Bearer test-token")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&serde_json::json!({
                            "limit_usd": 1.0,
                            "period": "monthly",
                            "hourly_limit_usd": null,
                            "per_run_reserve_usd": 0.5,
                            "time_cost_per_hour_usd": 3.0
                        }))
                        .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(post.status(), StatusCode::OK);

        let get = app
            .oneshot(
                Request::builder()
                    .uri("/autopilot/budget")
                    .header("Authorization", "Bearer test-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(get.status(), StatusCode::OK);
        let body = axum::body::to_bytes(get.into_body(), usize::MAX)
            .await
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(parsed["paused"], serde_json::json!(true));
        assert_eq!(parsed["window_spend_usd"], serde_json::json!(5.0));
        assert!(parsed["reason"].is_string());
    }

    #[tokio::test]
    async fn autopilot_kill_scoped_get_is_empty_by_default() {
        let app = build_router(test_state().await);
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/autopilot/kill/scoped")
                    .header("Authorization", "Bearer test-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(parsed, serde_json::json!([]));
    }

    #[tokio::test]
    async fn autopilot_kill_scoped_post_then_get_reflects_it() {
        let app = build_router(test_state().await);
        let post = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/autopilot/kill/scoped")
                    .header("Authorization", "Bearer test-token")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&serde_json::json!({
                            "scope_type": "project",
                            "scope_id": "alpha",
                            "engaged": true
                        }))
                        .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(post.status(), StatusCode::NO_CONTENT);

        let get = app
            .oneshot(
                Request::builder()
                    .uri("/autopilot/kill/scoped")
                    .header("Authorization", "Bearer test-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(get.status(), StatusCode::OK);
        let body = axum::body::to_bytes(get.into_body(), usize::MAX)
            .await
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            parsed,
            serde_json::json!([
                { "scope_type": "project", "scope_id": "alpha", "engaged": true }
            ])
        );
    }

    #[tokio::test]
    async fn autopilot_kill_scoped_post_rejects_invalid_scope_type() {
        let app = build_router(test_state().await);
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/autopilot/kill/scoped")
                    .header("Authorization", "Bearer test-token")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&serde_json::json!({
                            "scope_type": "bogus",
                            "scope_id": "x",
                            "engaged": true
                        }))
                        .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    /// Seeds one shadow-mode run whose `read-local` class has `reviewed` approved decisions plus one
    /// still-unreviewed decision, and returns that unreviewed decision's id.
    async fn seed_shadow_class(pool: &sqlx::SqlitePool, project_id: &str, reviewed: usize) -> i64 {
        sqlx::query(
            "INSERT INTO runs (project_id, prompt, status, mode, created_at)
             VALUES (?, 'seed', 'completed', 'shadow', '2026-07-27T00:00:00Z')",
        )
        .bind(project_id)
        .execute(pool)
        .await
        .unwrap();
        let run_id: i64 = sqlx::query_scalar("SELECT last_insert_rowid()")
            .fetch_one(pool)
            .await
            .unwrap();

        let mut last = 0;
        for index in 0..=reviewed {
            let verdict = if index < reviewed {
                Some("approve")
            } else {
                None
            };
            sqlx::query(
                "INSERT INTO shadow_decisions
                 (run_id, tool_name, tool_input, decision, reason, action_class,
                  classifier_version, human_verdict, reviewed_at, created_at)
                 VALUES (?, 'Read', '{}', 'allow', 'seed', 'read-local', 1, ?, NULL,
                         '2026-07-27T00:00:00Z')",
            )
            .bind(run_id)
            .bind(verdict)
            .execute(pool)
            .await
            .unwrap();
            last = sqlx::query_scalar("SELECT last_insert_rowid()")
                .fetch_one(pool)
                .await
                .unwrap();
        }
        last
    }

    async fn post_verdict(state: AppState, id: i64) -> StatusCode {
        build_router(state)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/shadow-decisions/{id}/verdict"))
                    .header("Authorization", "Bearer test-token")
                    .header("Content-Type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&serde_json::json!({ "verdict": "approve" })).unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap()
            .status()
    }

    async fn promotion_feed_rows(pool: &sqlx::SqlitePool) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM feed WHERE kind = 'promotion_ready'")
            .fetch_one(pool)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn verdict_that_clears_the_bar_announces_the_project_as_promotable() {
        let state = test_state().await;
        let pool = state.pool.clone();
        // Nine reviewed leaves the class one short of the ten-review floor.
        let last = seed_shadow_class(&pool, "project-a", 9).await;

        assert_eq!(promotion_feed_rows(&pool).await, 0);
        assert_eq!(post_verdict(state, last).await, StatusCode::NO_CONTENT);

        assert_eq!(promotion_feed_rows(&pool).await, 1);
        let summary: String =
            sqlx::query_scalar("SELECT summary FROM feed WHERE kind = 'promotion_ready'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert!(summary.contains("project-a"), "got: {summary}");
    }

    #[tokio::test]
    async fn verdict_below_the_bar_announces_nothing() {
        let state = test_state().await;
        let pool = state.pool.clone();
        let last = seed_shadow_class(&pool, "project-a", 3).await;

        assert_eq!(post_verdict(state, last).await, StatusCode::NO_CONTENT);

        assert_eq!(promotion_feed_rows(&pool).await, 0);
    }

    #[tokio::test]
    async fn an_already_promotable_project_is_not_announced_again() {
        let state = test_state().await;
        let pool = state.pool.clone();
        // Ten reviewed already clears the bar, so the eleventh verdict is not a crossing.
        let last = seed_shadow_class(&pool, "project-a", 10).await;

        assert_eq!(post_verdict(state, last).await, StatusCode::NO_CONTENT);

        assert_eq!(promotion_feed_rows(&pool).await, 0);
    }
}
