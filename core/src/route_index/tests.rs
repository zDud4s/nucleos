use super::*;
use crate::auth::ApiTokenLevel;
use axum::body::Body as HttpBody;
use axum::http::{Method, Request};
use tower::ServiceExt;

const fn entry(method: &'static str, path: &'static str, purpose: &'static str) -> RouteEntry {
    RouteEntry {
        method,
        path,
        purpose,
        body: Body::None,
        handler: "h",
    }
}

const HOT_PAIRS: [(&str, &str); 6] = [
    ("POST", "/runs"),
    ("POST", "/jobs"),
    ("POST", "/runs/{id}/message"),
    ("GET", "/runs"),
    ("POST", "/proposals/{id}/approve"),
    ("POST", "/runs/{id}/cancel"),
];

fn find(method: &str, path: &str) -> &'static RouteEntry {
    ROUTES
        .iter()
        .find(|e| e.method == method && e.path == path)
        .expect("route in ROUTES")
}

#[test]
fn search_ranks_a_path_hit_above_a_purpose_hit() {
    let table = [
        entry("GET", "/other", "lists every widget"),
        entry("GET", "/widget", "something unrelated"),
    ];
    let hits = search(&table, "widget", 10);
    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0].path, "/widget");
}

#[test]
fn search_requires_every_token_to_match() {
    let table = [
        entry("POST", "/runs/{id}/cancel", "stop a run"),
        entry("POST", "/runs", "create a run"),
    ];
    let hits = search(&table, "RUNS cancel", 10);
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].path, "/runs/{id}/cancel");
}

#[test]
fn search_honours_the_limit() {
    let table = [
        entry("GET", "/a", "x"),
        entry("GET", "/ab", "x"),
        entry("GET", "/abc", "x"),
    ];
    assert_eq!(search(&table, "a", 2).len(), 2);
}

#[test]
fn limit_defaults_to_ten_and_clamps_to_one_through_fifty() {
    assert_eq!(clamp_limit(None), 10);
    assert_eq!(clamp_limit(Some(0)), 1);
    assert_eq!(clamp_limit(Some(-5)), 1);
    assert_eq!(clamp_limit(Some(7)), 7);
    assert_eq!(clamp_limit(Some(9999)), 50);
}

#[test]
fn authority_is_the_lowest_level_that_permits_the_route() {
    assert_eq!(
        authority_of(&entry("GET", "/status", "")),
        ApiTokenLevel::ReadOnly
    );
    assert_eq!(
        authority_of(&entry("POST", "/runs", "")),
        ApiTokenLevel::RunCreating
    );
    assert_eq!(
        authority_of(&entry("POST", "/daemon/shutdown", "")),
        ApiTokenLevel::Admin
    );
}

#[test]
fn the_search_route_itself_is_read_only() {
    // CR2: `GET /routes/search` sits in `READ_ONLY_ROUTES`.
    assert_eq!(
        authority_of(find("GET", "/routes/search")),
        ApiTokenLevel::ReadOnly
    );
}

#[test]
fn routes_contains_the_search_route_and_the_six_hot_paths() {
    find("GET", "/routes/search");
    for (method, path) in HOT_PAIRS {
        find(method, path);
    }
    // The generated table holds every daemon route, not only these seven.
    assert!(ROUTES.len() >= 7);
}

#[test]
fn every_hot_op_has_an_entry_in_routes() {
    let ops = hot_ops();
    assert_eq!(ops.len(), 6);
    for op in ops {
        find(op.method, op.path);
    }
}

#[test]
fn each_hot_example_deserialises_into_its_request_type() {
    let ops = hot_ops();
    assert_eq!(ops.len(), 6);
    for op in ops {
        let ex = op.example.clone();
        match (op.method, op.path) {
            ("POST", "/runs") => {
                serde_json::from_value::<crate::execution::runs::CreateRunRequest>(ex).unwrap();
            }
            ("POST", "/jobs") => {
                serde_json::from_value::<crate::execution::job::CreateJobRequest>(ex).unwrap();
            }
            ("POST", "/runs/{id}/message") => {
                // `RunMessageRequest` is private to `http`; its only field is `message`.
                assert!(ex["message"].is_string());
            }
            ("POST", "/proposals/{id}/approve") => assert!(ex.is_object()),
            _ => {}
        }
    }
}

#[test]
fn the_prompt_block_names_all_six_method_path_pairs() {
    let block = render_prompt_block();
    for (method, path) in HOT_PAIRS {
        assert!(
            block.contains(&format!("{method} {path}")),
            "missing {method} {path}"
        );
    }
}

#[test]
fn post_runs_body_is_a_schema() {
    assert_eq!(find("POST", "/runs").body, Body::Schema("CreateRunRequest"));
    assert!(schema_of("CreateRunRequest").is_some());
    assert_eq!(hot_ops().len(), 6);
}

async fn get_search(uri: &str) -> (axum::http::StatusCode, serde_json::Value) {
    let state = crate::channels::testkit::state().await;
    // `http` lives in the binary, so the module's own router is exercised without the token layer.
    let app = routes().with_state(state);
    let response = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri(uri)
                .body(HttpBody::empty())
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
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
    )
}

#[tokio::test]
async fn an_empty_query_is_a_400() {
    assert_eq!(get_search("/routes/search?q=").await.0, 400);
    assert_eq!(get_search("/routes/search").await.0, 400);
}

#[tokio::test]
async fn a_query_answers_items_with_method_path_authority_purpose_and_body() {
    let (status, json) = get_search("/routes/search?q=runs%20cancel&limit=50").await;
    assert_eq!(status, 200);
    let item = json
        .as_array()
        .expect("array")
        .iter()
        .find(|i| i["path"] == "/runs/{id}/cancel")
        .expect("cancel route in the hits");
    assert_eq!(item["method"], "POST");
    assert_eq!(item["path"], "/runs/{id}/cancel");
    // `POST /runs/{id}/cancel` is in neither `READ_ONLY_ROUTES` nor `RUN_CREATING_ROUTES`, so only
    // Admin (and Control) may stop a run: a run-creating key buys runs, it does not kill them.
    assert_eq!(item["authority"], "admin");
    assert_eq!(item["body"]["state"], "none");
    assert!(item["purpose"].is_string());
}
