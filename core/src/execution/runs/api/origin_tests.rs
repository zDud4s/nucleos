//! C3: `x-nucleos-origin: voice` stamps `runs.origin`; nothing else does.

use super::*;
use crate::execution::runs::testkit::test_state_with_runner;
use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, HeaderValue, Request};
use axum::routing::post;
use tower::ServiceExt;

async fn state() -> AppState {
    test_state_with_runner(None, crate::state::DEFAULT_RUN_TIMEOUT)
        .await
        .0
}

fn app(state: AppState) -> Router {
    Router::new()
        .route("/runs", post(create_run_route))
        .with_state(state)
}

/// POSTs a run, optionally with the origin header; returns status and raw body.
async fn post_run(app: &Router, origin: Option<&str>) -> (StatusCode, serde_json::Value) {
    let mut builder = Request::builder()
        .method("POST")
        .uri("/runs")
        .header("content-type", "application/json");
    if let Some(value) = origin {
        builder = builder.header(ORIGIN_HEADER, value);
    }
    let response = app
        .clone()
        .oneshot(builder.body(Body::from(r#"{"prompt":"hello"}"#)).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, serde_json::from_slice(&body).unwrap())
}

async fn origin_of(pool: &sqlx::SqlitePool, id: i64) -> Option<String> {
    sqlx::query_scalar::<_, Option<String>>("SELECT origin FROM runs WHERE id = ?")
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn created_with(origin: Option<&str>) -> Option<String> {
    let state = state().await;
    let pool = state.pool.clone();
    let (status, body) = post_run(&app(state), origin).await;
    assert_eq!(status, StatusCode::OK);
    origin_of(&pool, body["id"].as_i64().unwrap()).await
}

#[test]
fn the_header_name_is_the_documented_one() {
    assert_eq!(ORIGIN_HEADER, "x-nucleos-origin");
}

#[tokio::test]
async fn header_voice_stamps_the_run_voice() {
    assert_eq!(created_with(Some("voice")).await.as_deref(), Some("voice"));
}

#[tokio::test]
async fn no_header_leaves_origin_null() {
    assert_eq!(created_with(None).await, None);
}

#[tokio::test]
async fn other_header_values_leave_origin_null() {
    for value in [
        "shell",
        "telegram",
        "channel:telegram",
        "VOICE",
        "garbage",
        "",
    ] {
        assert_eq!(created_with(Some(value)).await, None, "value {value:?}");
    }
}

#[tokio::test]
async fn tag_origin_does_not_overwrite_an_existing_origin() {
    let state = state().await;
    let pool = state.pool.clone();
    let (_, body) = post_run(&app(state), None).await;
    let id = body["id"].as_i64().unwrap();
    sqlx::query("UPDATE runs SET origin = 'shell' WHERE id = ?")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    let mut headers = HeaderMap::new();
    headers.insert(ORIGIN_HEADER, HeaderValue::from_static("voice"));
    tag_origin(&pool, id, &headers).await;
    assert_eq!(origin_of(&pool, id).await.as_deref(), Some("shell"));
}

#[tokio::test]
async fn response_status_and_shape_are_identical_with_and_without_header() {
    let state = state().await;
    let app = app(state);
    let (plain_status, plain_body) = post_run(&app, None).await;
    let (voice_status, voice_body) = post_run(&app, Some("voice")).await;
    assert_eq!(plain_status, voice_status);
    let keys = |body: &serde_json::Value| {
        body.as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<Vec<_>>()
    };
    assert_eq!(keys(&plain_body), keys(&voice_body));
    assert!(voice_body["id"].is_i64());
}
