//! The context-fit routes: `GET /trees/{id}/context-fit` (the card of every node of one tree) and
//! `GET /analytics/context-fit?days=N` (counts by verdict and layer, and the newest cards), thin
//! JSON over `crate::context_fit::store`. The shapes are a contract with the shell's views.
//!
//! Both are in `auth::READ_ONLY_ROUTES`: counts, signal ids and thresholds over tables a read-only
//! key already reads, no text and no paths.

use serde_json::{Value, json};

use super::*;

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/trees/{id}/context-fit", get(get_tree_context_fit))
        .route("/analytics/context-fit", get(get_analytics_context_fit))
}

type Refusal = (StatusCode, Json<Value>);

/// The longest window the analytics answer serves, in days.
const MAX_DAYS: u32 = 365;
/// The window when `days` is absent.
const DEFAULT_DAYS: u32 = 7;

fn refuse(status: StatusCode, message: &str) -> Refusal {
    (status, Json(json!({ "error": message })))
}

fn storage(error: sqlx::Error) -> Refusal {
    tracing::warn!(%error, "context fit read failed");
    refuse(StatusCode::INTERNAL_SERVER_ERROR, "context fit failed")
}

#[derive(Debug, Default, Deserialize)]
pub(super) struct FitQuery {
    days: Option<String>,
}

async fn get_tree_context_fit(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, Refusal> {
    let nodes = crate::context_fit::store::tree_rows(&state.pool, &id)
        .await
        .map_err(storage)?;
    Ok(Json(json!({ "tree_id": id, "nodes": nodes })))
}

async fn get_analytics_context_fit(
    State(state): State<AppState>,
    Query(query): Query<FitQuery>,
) -> Result<Json<crate::context_fit::store::Analytics>, Refusal> {
    let days = match query.days.as_deref() {
        None => DEFAULT_DAYS,
        Some(raw) => match raw.parse::<u32>() {
            Ok(days) if (1..=MAX_DAYS).contains(&days) => days,
            _ => {
                return Err(refuse(
                    StatusCode::BAD_REQUEST,
                    "days must be a whole number from 1 to 365",
                ));
            }
        },
    };
    crate::context_fit::store::analytics(&state.pool, days)
        .await
        .map(Json)
        .map_err(storage)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    async fn app_and_pool() -> (Router, sqlx::SqlitePool) {
        let (state, _) = crate::execution::runs::testkit::test_state_with_runner(
            None,
            crate::state::DEFAULT_RUN_TIMEOUT,
        )
        .await;
        let pool = state.pool.clone();
        (crate::http::build_router(state), pool)
    }

    async fn get_at(app: &Router, uri: &str) -> (StatusCode, Value) {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(uri)
                    .header("Authorization", "Bearer test-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    async fn card(pool: &sqlx::SqlitePool, node: &str, tree: &str, layer: &str, verdicts: &str) {
        let evidence = if verdicts == "[]" {
            "[]"
        } else {
            r#"[{"signal":"u2","value":2.0,"threshold":1.0}]"#
        };
        sqlx::query(
            "INSERT INTO node_context_fit \
             (node_id, tree_id, layer, verdicts, evidence, given, used, rules_version, computed_at) \
             VALUES (?, ?, ?, ?, ?, '{\"pointers\":1}', '{\"tokens\":10}', 1, ?)",
        )
        .bind(node)
        .bind(tree)
        .bind(layer)
        .bind(verdicts)
        .bind(evidence)
        .bind(chrono::Utc::now().to_rfc3339())
        .execute(pool)
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn a_tree_answers_the_card_of_each_of_its_nodes() {
        let (app, pool) = app_and_pool().await;
        card(&pool, "n1", "t1", "core", r#"["under","split"]"#).await;
        card(&pool, "n2", "t1", "shell", "[]").await;
        card(&pool, "n3", "t2", "core", "[]").await;

        let (status, body) = get_at(&app, "/trees/t1/context-fit").await;

        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["tree_id"], "t1");
        let nodes = body["nodes"].as_array().unwrap();
        assert_eq!(nodes.len(), 2, "{body}");
        let first = nodes.iter().find(|n| n["node_id"] == "n1").unwrap();
        assert_eq!(first["tree_id"], "t1");
        assert_eq!(first["layer"], "core");
        assert_eq!(first["verdicts"], json!(["under", "split"]));
        assert_eq!(
            first["evidence"],
            json!([{"signal": "u2", "value": 2.0, "threshold": 1.0}])
        );
        assert_eq!(first["given"], json!({"pointers": 1}));
        assert_eq!(first["used"], json!({"tokens": 10}));
        assert_eq!(first["rules_version"], 1);
        assert!(first["computed_at"].is_string());
    }

    #[tokio::test]
    async fn an_unknown_tree_answers_an_empty_list() {
        let (app, _pool) = app_and_pool().await;

        let (status, body) = get_at(&app, "/trees/nobody/context-fit").await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, json!({"tree_id": "nobody", "nodes": []}));
    }

    #[tokio::test]
    async fn analytics_answers_the_documented_shape() {
        let (app, pool) = app_and_pool().await;
        card(&pool, "n1", "t1", "core", r#"["under","split"]"#).await;
        card(&pool, "n2", "t1", "shell", "[]").await;
        card(&pool, "n3", "t2", "core", "[]").await;

        let (status, body) = get_at(&app, "/analytics/context-fit").await;

        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["days"], 7);
        assert_eq!(body["nodes"], 3);
        assert_eq!(
            body["by_verdict"],
            json!({"under": 1, "over": 0, "split": 1, "fit": 2})
        );
        assert_eq!(
            body["by_layer"],
            json!([
                {"layer": "core", "under": 1, "over": 0, "split": 1, "fit": 1},
                {"layer": "shell", "under": 0, "over": 0, "split": 0, "fit": 1},
            ])
        );
        assert_eq!(body["recent"].as_array().unwrap().len(), 3);
        assert!(body["recent"][0]["node_id"].is_string());
    }

    #[tokio::test]
    async fn analytics_takes_a_window_and_refuses_a_bad_one() {
        let (app, pool) = app_and_pool().await;
        card(&pool, "n1", "t1", "core", "[]").await;
        sqlx::query("UPDATE node_context_fit SET computed_at = '2000-01-01T00:00:00+00:00'")
            .execute(&pool)
            .await
            .unwrap();

        let (_, narrow) = get_at(&app, "/analytics/context-fit?days=30").await;
        assert_eq!(narrow["days"], 30);
        assert_eq!(narrow["nodes"], 0);
        for bad in ["abc", "0", "-1", "366"] {
            let (status, _) = get_at(&app, &format!("/analytics/context-fit?days={bad}")).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "days={bad}");
        }
    }
}
