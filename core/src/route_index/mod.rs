//! The route index: every HTTP route the daemon serves, searchable by `GET /routes/search`.
use axum::Json;
use axum::Router;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use serde::Deserialize;

use crate::state::AppState;

mod generated;
// Only the tests read `hot` today; as a lib module its `pub` hid that, inside the binary it warns.
#[allow(dead_code)]
pub mod hot;
mod search;
#[cfg(test)]
mod tests;

pub use generated::{ROUTES, schema_of};
#[allow(unused_imports)]
pub use hot::{HotOp, hot_ops, render_prompt_block};
pub use search::{authority_of, clamp_limit, search};

/// What the request body of a route looks like.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Body {
    /// The handler takes no `Json<T>`.
    None,
    /// `T` derives `JsonSchema`; `schema_of(name)` yields its schema.
    Schema(&'static str),
    /// `T` exists but has no schema derive (or is not nameable from here).
    Shapeless(&'static str),
}

/// One route as the scanner found it.
#[derive(Clone, Copy, Debug)]
pub struct RouteEntry {
    pub method: &'static str,
    pub path: &'static str,
    pub purpose: &'static str,
    pub body: Body,
    // Written by the generator, read by nothing yet.
    #[allow(dead_code)]
    pub handler: &'static str,
}

#[derive(Deserialize)]
pub struct SearchParams {
    #[serde(default)]
    pub q: String,
    #[serde(default)]
    pub limit: Option<i64>,
}

pub fn routes() -> Router<AppState> {
    Router::new().route("/routes/search", get(search_route))
}

fn body_json(body: Body) -> serde_json::Value {
    match body {
        Body::None => serde_json::json!({ "state": "none" }),
        Body::Schema(name) => serde_json::json!({
            "state": "schema",
            "type": name,
            "schema": schema_of(name),
        }),
        Body::Shapeless(name) => serde_json::json!({ "state": "shapeless", "type": name }),
    }
}

async fn search_route(
    State(_state): State<AppState>,
    Query(params): Query<SearchParams>,
) -> Response {
    if params.q.trim().is_empty() {
        return (StatusCode::BAD_REQUEST, "q must not be empty").into_response();
    }
    let hits = search(ROUTES, &params.q, clamp_limit(params.limit));
    let items: Vec<serde_json::Value> = hits
        .into_iter()
        .map(|e| {
            serde_json::json!({
                "method": e.method,
                "path": e.path,
                "authority": authority_of(e).as_str(),
                "purpose": e.purpose,
                "body": body_json(e.body),
            })
        })
        .collect();
    Json(items).into_response()
}
