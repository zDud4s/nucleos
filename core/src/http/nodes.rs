//! Work-node routes (`/nodes*`): the HTTP face of `work_nodes`, the bounded tree of delegated work.
//!
//! The shapes are the fixed contract of the hierarchical-orchestration spec (section 13): the
//! Claude Code bridge's `SessionStart` hook, the shell's rotation and the `node_*` MCP tools all
//! call these routes, so they change only with that contract.
//!
//! Every refusal is `{error}` with the status `NodeError::status_code` names. A field sent as
//! `null` reads exactly as an absent one, because the MCP client sends every field it has.
//!
//! **Who reaches them.** No route here is in any `auth.rs` table, so only the control token and an
//! Admin API key get in — the same reach as `/hooks/bundle`, whose caller (the hook forwarder in a
//! person's own Claude Code session) is the main caller here too, alongside the MCP daemon client
//! of such a session. `Scope::Run` is deliberately left out: it only ever matches literal POST
//! paths, so it could not reach `PATCH /nodes/{id}` or the `GET` reads anyway, and a half-reachable
//! tree is worse than none. Letting an autonomous run keep its own tree is a later decision.

use axum::extract::rejection::{JsonRejection, QueryRejection};
use axum::routing::patch;
use serde_json::{Value, json};
use std::path::Path as FsPath;

use super::*;
use crate::work_nodes::{self, CloseArgs, NodeError, NodeLimits, OpenArgs, UpdateArgs, View};

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/nodes", post(post_node))
        // Literal siblings of `/nodes/{id}`: the router prefers a literal segment to a parameter,
        // so neither is ever read as a node id.
        .route("/nodes/active", get(get_active))
        .route("/nodes/tree/{root_id}", get(get_tree))
        .route("/nodes/{id}", patch(patch_node))
        .route("/nodes/{id}/close", post(post_close))
}

type Refusal = (StatusCode, Json<Value>);

fn refuse(status: StatusCode, message: impl Into<String>) -> Refusal {
    (status, Json(json!({ "error": message.into() })))
}

fn refused(error: NodeError) -> Refusal {
    if let NodeError::Db(db) = &error {
        tracing::warn!(error = %db, "work node storage failed");
    }
    refuse(error.status_code(), error.to_string())
}

fn bad_body(rejection: JsonRejection) -> Refusal {
    refuse(StatusCode::BAD_REQUEST, rejection.body_text())
}

/// A blank string is no value: `?session_id=` must not match rows whose session is empty.
fn present(value: Option<String>) -> Option<String> {
    value.filter(|text| !text.trim().is_empty())
}

/// The limits of the project a node belongs to, read from that project's root; the defaults when
/// the node has no project or its root cannot be found.
async fn limits_for(state: &AppState, project_id: Option<&str>) -> NodeLimits {
    let Some(project_id) = project_id else {
        return NodeLimits::load(None);
    };
    match inspect::project_root(&state.pool, project_id).await {
        Ok(Some(root)) => NodeLimits::load(Some(FsPath::new(&root))),
        Ok(None) => NodeLimits::load(None),
        Err(error) => {
            tracing::warn!(project = project_id, %error, "could not read the project root");
            NodeLimits::load(None)
        }
    }
}

#[derive(Debug, Default, Deserialize)]
struct OpenBody {
    parent_id: Option<String>,
    kind: Option<String>,
    title: Option<String>,
    goal: Option<String>,
    owned_paths: Option<Vec<String>>,
    budget_tokens: Option<i64>,
    task_id: Option<String>,
    session_id: Option<String>,
    chat_id: Option<String>,
}

fn required(field: &str, value: Option<String>) -> Result<String, Refusal> {
    value.ok_or_else(|| refuse(StatusCode::BAD_REQUEST, format!("{field} is required")))
}

/// `POST /nodes`: opens a root (no `parent_id`) or a child under a running node.
///
/// The project is best effort: a root opened by the owner has none (the body names no worktree),
/// and a child inherits its parent's, which is also where its limits are read from.
async fn post_node(
    State(state): State<AppState>,
    body: Result<Json<OpenBody>, JsonRejection>,
) -> Result<Json<work_nodes::Opened>, Refusal> {
    let Json(body) = body.map_err(bad_body)?;
    let args = OpenArgs {
        parent_id: present(body.parent_id),
        kind: required("kind", body.kind)?,
        title: required("title", body.title)?,
        goal: required("goal", body.goal)?,
        owned_paths: body.owned_paths.unwrap_or_default(),
        budget_tokens: body.budget_tokens,
        task_id: present(body.task_id),
        session_id: present(body.session_id),
        chat_id: present(body.chat_id),
        run_id: None,
    };
    let parent_project = match args.parent_id.as_deref() {
        // A missing parent is refused by `open` itself, with the same 404.
        Some(parent_id) => match work_nodes::get(&state.pool, parent_id).await {
            Ok(parent) => parent.project_id,
            Err(NodeError::NotFound) => None,
            Err(error) => return Err(refused(error)),
        },
        None => None,
    };
    let limits = limits_for(&state, parent_project.as_deref()).await;
    work_nodes::open(&state.pool, None, &limits, &args)
        .await
        .map(Json)
        .map_err(refused)
}

/// `PATCH /nodes/{id}`: `{status?, decision?, context_peak?}`; only what is sent changes.
async fn patch_node(
    State(state): State<AppState>,
    Path(id): Path<String>,
    body: Result<Json<UpdateArgs>, JsonRejection>,
) -> Result<Json<work_nodes::Node>, Refusal> {
    let Json(args) = body.map_err(bad_body)?;
    work_nodes::update(&state.pool, &id, &args)
        .await
        .map(Json)
        .map_err(refused)
}

#[derive(Debug, Default, Deserialize)]
struct CloseBody {
    status: Option<String>,
    summary: Option<String>,
    handoff_path: Option<String>,
}

/// `POST /nodes/{id}/close`: `{status, summary, handoff_path}`; 422 when the summary is too long
/// or the handoff file does not exist.
async fn post_close(
    State(state): State<AppState>,
    Path(id): Path<String>,
    body: Result<Json<CloseBody>, JsonRejection>,
) -> Result<Json<work_nodes::Node>, Refusal> {
    let Json(body) = body.map_err(bad_body)?;
    let args = CloseArgs {
        status: required("status", body.status)?,
        summary: required("summary", body.summary)?,
        handoff_path: required("handoff_path", body.handoff_path)?,
    };
    let node = work_nodes::get(&state.pool, &id).await.map_err(refused)?;
    let limits = limits_for(&state, node.project_id.as_deref()).await;
    let closed = work_nodes::close(&state.pool, &id, &limits, &args)
        .await
        .map_err(refused)?;
    announce_close(&state.pool, &closed).await;
    Ok(Json(closed))
}

/// Leaves the feed line a close earns (`work_nodes::feed_line`), if any.
///
/// Best effort, like the handoff `rework` record in `work_nodes::close`: the close is already
/// committed and is the source of truth, so a feed failure is logged and never undoes it.
async fn announce_close(pool: &sqlx::SqlitePool, node: &work_nodes::Node) {
    let root_title = if node.kind == "root" {
        node.title.clone()
    } else {
        match work_nodes::get(pool, &node.root_id).await {
            Ok(root) => root.title,
            Err(error) => {
                tracing::warn!(node = %node.id, %error, "could not read the root title for the feed");
                node.title.clone()
            }
        }
    };
    let Some((kind, summary)) = work_nodes::feed_line(&root_title, node) else {
        return;
    };
    if let Err(error) =
        crate::feed::append(pool, node.project_id.as_deref(), kind, &summary, None, None).await
    {
        tracing::warn!(node = %node.id, %error, "could not write the work tree feed line");
    }
}

#[derive(Debug, Default, Deserialize)]
struct TreeQuery {
    view: Option<String>,
}

/// `GET /nodes/tree/{root_id}?view=brief|full`; `brief` when absent.
async fn get_tree(
    State(state): State<AppState>,
    Path(root_id): Path<String>,
    query: Result<Query<TreeQuery>, QueryRejection>,
) -> Result<Json<work_nodes::Tree>, Refusal> {
    let Query(query) = query.map_err(|r| refuse(StatusCode::BAD_REQUEST, r.body_text()))?;
    let view = match present(query.view).as_deref().map(str::trim) {
        None | Some("brief") => View::Brief,
        Some("full") => View::Full,
        Some(other) => {
            return Err(refuse(
                StatusCode::BAD_REQUEST,
                format!("view must be \"brief\" or \"full\", not {other:?}"),
            ));
        }
    };
    work_nodes::tree(&state.pool, &root_id, view)
        .await
        .map(Json)
        .map_err(refused)
}

#[derive(Debug, Default, Deserialize)]
struct ActiveQuery {
    session_id: Option<String>,
    chat_id: Option<String>,
}

/// `GET /nodes/active?session_id=…` or `?chat_id=…`: `{root_id}` of the running root bound to it,
/// or 404. The `SessionStart` hook fails open on that 404, so it is an answer, not an error.
async fn get_active(
    State(state): State<AppState>,
    query: Result<Query<ActiveQuery>, QueryRejection>,
) -> Result<Json<Value>, Refusal> {
    let Query(query) = query.map_err(|r| refuse(StatusCode::BAD_REQUEST, r.body_text()))?;
    let session_id = present(query.session_id);
    let chat_id = present(query.chat_id);
    match work_nodes::active(&state.pool, session_id.as_deref(), chat_id.as_deref()).await {
        Ok(Some(root_id)) => Ok(Json(json!({ "root_id": root_id }))),
        Ok(None) => Err(refuse(
            StatusCode::NOT_FOUND,
            "no running work tree is bound to that session or chat",
        )),
        Err(error) => Err(refused(error)),
    }
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

    async fn app() -> Router {
        app_and_pool().await.0
    }

    /// Every tree line in the feed, oldest first: `(kind, summary, project_id, run_id)`.
    async fn tree_lines(
        pool: &sqlx::SqlitePool,
    ) -> Vec<(String, String, Option<String>, Option<i64>)> {
        sqlx::query_as(
            "SELECT kind, summary, project_id, run_id FROM feed \
             WHERE kind LIKE 'work_tree_%' ORDER BY id",
        )
        .fetch_all(pool)
        .await
        .unwrap()
    }

    async fn close_node(
        app: &Router,
        id: &str,
        status: &str,
        summary: &str,
    ) -> (StatusCode, Value) {
        call(
            app,
            "POST",
            &format!("/nodes/{id}/close"),
            Some(json!({ "status": status, "summary": summary, "handoff_path": handoff_file() })),
        )
        .await
    }

    async fn call(
        app: &Router,
        method: &str,
        uri: &str,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let mut request = Request::builder()
            .method(method)
            .uri(uri)
            .header("Authorization", "Bearer test-token");
        let body = match body {
            Some(value) => {
                request = request.header("Content-Type", "application/json");
                Body::from(value.to_string())
            }
            None => Body::empty(),
        };
        let response = app
            .clone()
            .oneshot(request.body(body).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, value)
    }

    async fn open_root(app: &Router, session: &str) -> String {
        let (status, body) = call(
            app,
            "POST",
            "/nodes",
            Some(json!({
                "parent_id": null, "kind": "root", "title": "Root", "goal": "Ship the tree",
                "owned_paths": null, "budget_tokens": 200000, "task_id": null,
                "session_id": session, "chat_id": null,
            })),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["depth"], 0);
        body["id"].as_str().unwrap().to_owned()
    }

    async fn open_leaf(
        app: &Router,
        parent: &str,
        title: &str,
        owned: &[&str],
    ) -> (StatusCode, Value) {
        call(
            app,
            "POST",
            "/nodes",
            Some(json!({
                "parent_id": parent, "kind": "leaf", "title": title, "goal": "Do one part",
                "owned_paths": owned, "budget_tokens": null,
            })),
        )
        .await
    }

    fn handoff_file() -> String {
        let path = std::env::temp_dir().join(format!(
            "nucleos-node-handoff-{}.md",
            crate::auth::generate_uuid_v4()
        ));
        std::fs::write(&path, "# handoff\n").unwrap();
        path.to_string_lossy().into_owned()
    }

    #[tokio::test]
    async fn a_tree_opens_is_found_by_its_session_and_closes() {
        let app = app().await;
        let root = open_root(&app, "s-happy").await;

        let (status, body) = call(&app, "GET", "/nodes/active?session_id=s-happy", None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["root_id"], root.as_str());

        let (status, leaf) = open_leaf(&app, &root, "Leaf", &["core/src/a/**"]).await;
        assert_eq!(status, StatusCode::OK, "{leaf}");
        assert_eq!(leaf["root_id"], root.as_str());
        assert_eq!(leaf["depth"], 1);
        let leaf_id = leaf["id"].as_str().unwrap().to_owned();

        let (status, node) = call(
            &app,
            "PATCH",
            &format!("/nodes/{leaf_id}"),
            Some(json!({ "status": null, "decision": "split by module", "context_peak": 1000 })),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{node}");
        assert_eq!(node["status"], "running");
        assert_eq!(node["decisions"], json!(["split by module"]));

        let (status, closed) = call(
            &app,
            "POST",
            &format!("/nodes/{leaf_id}/close"),
            Some(json!({ "status": "done", "summary": "Done.", "handoff_path": handoff_file() })),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{closed}");
        assert_eq!(closed["status"], "done");

        let (status, tree) =
            call(&app, "GET", &format!("/nodes/tree/{root}?view=brief"), None).await;
        assert_eq!(status, StatusCode::OK, "{tree}");
        assert_eq!(tree["root_id"], root.as_str());
        assert_eq!(tree["goal"], "Ship the tree");
        let nodes = tree["nodes"].as_array().unwrap();
        assert_eq!(nodes.len(), 2);
        assert_eq!(nodes[1]["status"], "done");
        assert_eq!(nodes[1]["summary"], "Done.");
        assert!(!tree["text"].as_str().unwrap().is_empty());

        let (status, _) = call(&app, "GET", &format!("/nodes/tree/{root}?view=full"), None).await;
        assert_eq!(status, StatusCode::OK);
        let (status, _) = call(&app, "GET", &format!("/nodes/tree/{root}?view=wide"), None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn a_seventh_open_child_is_a_conflict() {
        let app = app().await;
        let root = open_root(&app, "s-width").await;
        for n in 0..6 {
            let (status, body) =
                open_leaf(&app, &root, &format!("Leaf {n}"), &[&format!("part{n}/**")]).await;
            assert_eq!(status, StatusCode::OK, "{body}");
        }
        let (status, body) = open_leaf(&app, &root, "Leaf 6", &["part6/**"]).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert!(body["error"].is_string());
    }

    #[tokio::test]
    async fn overlapping_paths_of_running_siblings_are_a_conflict() {
        let app = app().await;
        let root = open_root(&app, "s-overlap").await;
        let (status, body) = open_leaf(&app, &root, "Wide", &["core/**"]).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let (status, body) = open_leaf(&app, &root, "Narrow", &["core/src/x.rs"]).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert!(body["error"].as_str().unwrap().contains("overlaps"));
    }

    #[tokio::test]
    async fn a_summary_over_the_limit_is_unprocessable() {
        let app = app().await;
        let root = open_root(&app, "s-summary").await;
        let (status, body) = call(
            &app,
            "POST",
            &format!("/nodes/{root}/close"),
            Some(json!({
                "status": "done", "summary": "x".repeat(1501), "handoff_path": handoff_file(),
            })),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        assert!(body["error"].is_string());
    }

    #[tokio::test]
    async fn no_running_root_for_a_session_is_not_found() {
        let app = app().await;
        let (status, body) = call(&app, "GET", "/nodes/active?session_id=nobody", None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(body["error"].is_string());
        let (status, _) = call(&app, "GET", "/nodes/active", None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let (status, _) = call(&app, "GET", "/nodes/tree/missing", None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn closing_a_root_done_writes_one_finished_line() {
        let (app, pool) = app_and_pool().await;
        let root = open_root(&app, "s-feed-done").await;

        let (status, body) = close_node(&app, &root, "done", "Everything merged.").await;
        assert_eq!(status, StatusCode::OK, "{body}");

        let lines = tree_lines(&pool).await;
        assert_eq!(lines.len(), 1, "{lines:?}");
        let (kind, summary, _project, run_id) = &lines[0];
        assert_eq!(kind, "work_tree_finished");
        assert_eq!(summary, "Root: done — Everything merged.");
        assert_eq!(*run_id, None);
    }

    #[tokio::test]
    async fn closing_a_leaf_blocked_writes_a_parked_line_with_its_decisions() {
        let (app, pool) = app_and_pool().await;
        let root = open_root(&app, "s-feed-parked").await;
        let (status, leaf) = open_leaf(&app, &root, "Leaf", &["core/src/a/**"]).await;
        assert_eq!(status, StatusCode::OK, "{leaf}");
        let leaf_id = leaf["id"].as_str().unwrap().to_owned();
        let (status, node) = call(
            &app,
            "PATCH",
            &format!("/nodes/{leaf_id}"),
            Some(json!({ "status": null, "decision": "pick A or B", "context_peak": null })),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{node}");

        let (status, body) = close_node(&app, &leaf_id, "blocked", "Needs a call.").await;
        assert_eq!(status, StatusCode::OK, "{body}");

        let lines = tree_lines(&pool).await;
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert_eq!(lines[0].0, "work_tree_parked");
        // The title is the root's, not the leaf's.
        assert_eq!(
            lines[0].1,
            "Root: blocked — Needs a call. — precisa de: pick A or B"
        );
    }

    #[tokio::test]
    async fn closing_a_leaf_done_writes_nothing() {
        let (app, pool) = app_and_pool().await;
        let root = open_root(&app, "s-feed-leaf").await;
        let (status, leaf) = open_leaf(&app, &root, "Leaf", &["core/src/a/**"]).await;
        assert_eq!(status, StatusCode::OK, "{leaf}");
        let leaf_id = leaf["id"].as_str().unwrap().to_owned();

        let (status, body) = close_node(&app, &leaf_id, "done", "Done.").await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(tree_lines(&pool).await.is_empty());
    }

    #[tokio::test]
    async fn closing_a_root_handoff_writes_nothing() {
        let (app, pool) = app_and_pool().await;
        let root = open_root(&app, "s-feed-handoff").await;

        let (status, body) = close_node(&app, &root, "handoff", "Context is full.").await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(tree_lines(&pool).await.is_empty());
    }

    #[tokio::test]
    async fn a_refused_close_writes_nothing() {
        let (app, pool) = app_and_pool().await;
        let root = open_root(&app, "s-feed-refused").await;

        let (status, _) = close_node(&app, &root, "done", &"x".repeat(1501)).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(tree_lines(&pool).await.is_empty());
    }
}
