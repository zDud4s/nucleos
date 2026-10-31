//! HTTP routes for tree spending and cache observations.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, put};
use axum::{Json, Router};
use chrono::Utc;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::state::AppState;

#[derive(Debug, Deserialize)]
struct BudgetRequest {
    limit_usd: f64,
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/trees/{id}/cost", get(get_cost))
        .route("/trees/{id}/budget", put(put_budget))
        .route("/cost/cache-report", get(get_cache_report))
        .route(
            "/cost/wait-advice",
            get(crate::tree_cost::wait_policy::wait_advice_handler),
        )
}

/// Budgets, spending and refusal key on the tree's root node id, but callers also know the tree
/// job id: a job id maps to its root node id, anything else is taken as a root id already.
async fn resolve_root_id(pool: &sqlx::SqlitePool, id: &str) -> Result<String, StatusCode> {
    let root: Option<String> =
        sqlx::query_scalar("SELECT root_node_id FROM tree_jobs WHERE id = ?")
            .bind(id)
            .fetch_optional(pool)
            .await
            .map_err(|error| {
                tracing::warn!(%error, "could not resolve tree id");
                StatusCode::INTERNAL_SERVER_ERROR
            })?;
    Ok(root.filter(|root| !root.is_empty()).unwrap_or_else(|| id.to_owned()))
}

async fn get_cost(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, StatusCode> {
    let id = resolve_root_id(&state.pool, &id).await?;
    let spend = crate::tree_cost::budget::tree_spend(&state.pool, &id, Utc::now())
        .await
        .map_err(|error| {
            tracing::warn!(%error, "could not read tree cost");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
    Ok(Json(json!({
        "spent_usd": spend.total_usd,
        "budget": {
            "limit_usd": spend.limit_usd,
            "soft_pct": spend.soft_pct,
            "soft_crossed": spend.soft_crossed,
            "hard_crossed": spend.hard_crossed,
        },
    })))
}

async fn put_budget(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(request): Json<BudgetRequest>,
) -> Result<Json<Value>, StatusCode> {
    let id = resolve_root_id(&state.pool, &id).await?;
    let now = Utc::now().to_rfc3339();
    sqlx::query(
        "INSERT INTO tree_budgets (tree_id, limit_usd, soft_pct, created_at, updated_at) \
         VALUES (?, ?, 80.0, ?, ?) \
         ON CONFLICT(tree_id) DO UPDATE SET limit_usd = excluded.limit_usd, updated_at = excluded.updated_at",
    )
    .bind(&id)
    .bind(request.limit_usd)
    .bind(&now)
    .bind(&now)
    .execute(&state.pool)
    .await
    .map_err(|error| {
        tracing::warn!(%error, "could not save tree budget");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;
    Ok(Json(json!({ "limit_usd": request.limit_usd })))
}

async fn get_cache_report(State(state): State<AppState>) -> Result<Json<Value>, StatusCode> {
    let report = crate::tree_cost::report::cache_report(&state.pool)
        .await
        .map_err(|error| {
            tracing::warn!(%error, "could not build cache report");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
    Ok(Json(json!({
        "has_data": report.has_data,
        "effective_ttl": report.effective_ttl_s.value,
        "effective_ttl_estimated": report.effective_ttl_s.estimated,
        "p_resume": report.p_resume,
    })))
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

    /// Links job `job` to root node `root` and gives the root one costed run of `cost` USD.
    async fn seed_job_with_spend(state: &AppState, job: &str, root: &str, cost: f64) {
        sqlx::query(
            "INSERT INTO tree_jobs (id, root_node_id, goal, state, manifest, created_at, updated_at) \
             VALUES (?, ?, 'g', 'open', '{}', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
        )
        .bind(job)
        .bind(root)
        .execute(&state.pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO runs (id, prompt, status, mode, cost_usd, created_at) \
             VALUES (902, 'costed run', 'completed', 'shadow', ?, '2026-10-30T00:00:00Z')",
        )
        .bind(cost)
        .execute(&state.pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO work_nodes (id, root_id, depth, kind, title, goal, status, run_id, created_at, updated_at) \
             VALUES (?, ?, 0, 'root', 'title', 'goal', 'done', '902', \
                     '2026-10-30T00:00:00Z', '2026-10-30T00:00:00Z')",
        )
        .bind(root)
        .bind(root)
        .execute(&state.pool)
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn a_budget_set_by_job_id_applies_to_the_trees_root() {
        let state = state().await;
        let app = app(&state);
        seed_job_with_spend(&state, "proof-slugify-3", "root-uuid-1", 2.5).await;

        let (status, saved) = request(
            &app,
            "PUT",
            "/trees/proof-slugify-3/budget",
            json!({ "limit_usd": 7.0 }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{saved:?}");

        let (status, by_root) =
            request(&app, "GET", "/trees/root-uuid-1/cost", Value::Null).await;
        assert_eq!(status, StatusCode::OK, "{by_root:?}");
        assert_eq!(by_root["budget"]["limit_usd"], 7.0, "{by_root:?}");

        let (status, by_job) =
            request(&app, "GET", "/trees/proof-slugify-3/cost", Value::Null).await;
        assert_eq!(status, StatusCode::OK, "{by_job:?}");
        assert_eq!(by_job["budget"]["limit_usd"], 7.0, "{by_job:?}");
        assert_eq!(by_job["spent_usd"], 2.5, "{by_job:?}");
    }

    #[tokio::test]
    async fn a_budget_set_by_root_id_still_applies() {
        let state = state().await;
        let app = app(&state);
        seed_job_with_spend(&state, "proof-slugify-3", "root-uuid-1", 2.5).await;

        let (status, saved) = request(
            &app,
            "PUT",
            "/trees/root-uuid-1/budget",
            json!({ "limit_usd": 9.0 }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{saved:?}");

        let (status, cost) = request(&app, "GET", "/trees/root-uuid-1/cost", Value::Null).await;
        assert_eq!(status, StatusCode::OK, "{cost:?}");
        assert_eq!(cost["budget"]["limit_usd"], 9.0, "{cost:?}");
        assert_eq!(cost["spent_usd"], 2.5, "{cost:?}");
    }

    #[tokio::test]
    async fn cache_report_route_with_no_data() {
        let state = state().await;

        let (status, report) =
            request(&app(&state), "GET", "/cost/cache-report", Value::Null).await;

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
