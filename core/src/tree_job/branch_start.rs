//! Tree-job branch start: HTTP routes for starting a branch (scaffold, no routes yet).

use axum::Router;

use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use axum::{
        body::Body,
        http::{Request, StatusCode},
        Router,
    };
    use serde_json::{json, Value};
    use tower::ServiceExt;

    use super::*;
    use crate::execution::git_exec::testkit::{initialize_repo, space_free_tempdir};

    const TREE: &str = "branch-start-tree";

    async fn state() -> AppState {
        let (state, runner) = crate::execution::runs::testkit::test_state_with_runner(
            Some(Duration::from_secs(5)),
            Duration::from_secs(600),
        )
        .await;
        runner.canned.lock().unwrap().as_mut().unwrap().session_id = None;
        state
    }

    fn app(state: &AppState) -> Router {
        super::super::http::routes().with_state(state.clone())
    }

    async fn post(app: &Router, uri: &str, body: Value) -> (StatusCode, Value) {
        let request = Request::builder()
            .method("POST")
            .uri(uri)
            .header("Authorization", "Bearer test-token")
            .header("Content-Type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        let response = app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, serde_json::from_slice(&body).unwrap_or(Value::Null))
    }

    /// A temp git repository with one commit and a zero disk floor, so that a branch worktree is
    /// admitted whatever the disk holds. The layout `http.rs`'s `Repo` has.
    struct Repo {
        _container: tempfile::TempDir,
        path: std::path::PathBuf,
    }

    impl Repo {
        fn new() -> Repo {
            let container = space_free_tempdir("branch-start-");
            let path = container.path().join("repo");
            initialize_repo(&path);
            let dir = path.join(".nucleos/workflow");
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("speed.yaml"), "tree:\n  floor_gb: 0\n").unwrap();
            Repo {
                _container: container,
                path,
            }
        }
    }

    /// A tree job over a real repository, and the id of its root node, the caller of the tool.
    async fn tree() -> (AppState, Router, String, Repo) {
        let state = state().await;
        let app = app(&state);
        let repo = Repo::new();
        let (_, created) = post(
            &app,
            "/trees",
            json!({
                "id": TREE,
                "goal": "Ship it",
                "feature_branch": "feat/branch-start",
                "project_root": repo.path.to_string_lossy().replace('\\', "/"),
                "project_id": "project-0",
            }),
        )
        .await;
        let caller = created["root_node_id"]
            .as_str()
            .expect("the tree route creates its root node")
            .to_owned();
        (state, app, caller, repo)
    }

    async fn count(state: &AppState, table: &str) -> i64 {
        sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT COUNT(*) FROM {table}")))
            .fetch_one(&state.pool)
            .await
            .unwrap()
    }

    fn body(caller: &str) -> Value {
        json!({
            "name": "notes",
            "goal": "Write the branch notes",
            "owned_paths": [".ai/notes/**"],
            "prompt": "Write the notes.",
            "layer": "docs",
            "node_id": caller,
        })
    }

    fn uri() -> String {
        format!("/trees/{TREE}/branch_start")
    }

    #[tokio::test]
    async fn branch_start_adds_opens_binds_and_launches_in_one_call() {
        let (state, app, caller, _repo) = tree().await;
        let nodes = count(&state, "work_nodes").await;
        let runs = count(&state, "runs").await;

        let (status, response) = post(&app, &uri(), body(&caller)).await;

        assert_eq!(status, StatusCode::OK, "{response:?}");
        assert_eq!(response["branch"], "notes");
        assert!(response["node_id"].is_string(), "{response:?}");
        assert!(response["run_id"].is_i64(), "{response:?}");
        assert!(response["worktree"].is_string(), "{response:?}");
        assert_eq!(response["replayed"], false);
        assert_eq!(count(&state, "work_nodes").await, nodes + 1);
        assert_eq!(count(&state, "runs").await, runs + 1);
        let job = crate::tree_job::store::load(&state.pool, TREE)
            .await
            .unwrap()
            .unwrap();
        let branch = &job.manifest.branches["notes"];
        assert_eq!(branch.node_id.as_deref(), response["node_id"].as_str());
    }

    #[tokio::test]
    async fn branch_start_called_twice_makes_one_branch_one_node_one_run() {
        let (state, app, caller, _repo) = tree().await;
        let nodes = count(&state, "work_nodes").await;
        let runs = count(&state, "runs").await;
        let first = post(&app, &uri(), body(&caller)).await;
        let second = post(&app, &uri(), body(&caller)).await;
        assert_eq!(count(&state, "work_nodes").await, nodes + 1);
        assert_eq!(count(&state, "runs").await, runs + 1);

        assert_eq!(first.0, StatusCode::OK, "{:?}", first.1);
        assert_eq!(second.0, StatusCode::OK, "{:?}", second.1);
        assert_eq!(second.1["branch"], first.1["branch"]);
        assert_eq!(second.1["node_id"], first.1["node_id"]);
        assert_eq!(second.1["run_id"], first.1["run_id"]);
        assert_eq!(second.1["replayed"], true);
    }

    #[tokio::test]
    async fn branch_start_after_a_crash_between_steps_completes_the_missing_ones() {
        let (state, app, caller, _repo) = tree().await;
        // The first step alone: a branch with no worktree, no node and no run, as a crash leaves it.
        let nodes = count(&state, "work_nodes").await;
        let runs = count(&state, "runs").await;
        let added = post(
            &app,
            &format!("/trees/{TREE}/branches"),
            json!({ "name": "notes", "paths": [".ai/notes/**"], "layer": "docs" }),
        )
        .await;
        assert_eq!(added.0, StatusCode::OK, "{:?}", added.1);

        let resumed = post(&app, &uri(), body(&caller)).await;
        assert_eq!(resumed.0, StatusCode::OK, "{:?}", resumed.1);
        assert_eq!(resumed.1["branch"], "notes");
        assert!(resumed.1["node_id"].is_string(), "{:?}", resumed.1);
        assert!(resumed.1["run_id"].is_i64(), "{:?}", resumed.1);
        assert_eq!(resumed.1["replayed"], false);
        assert_eq!(count(&state, "work_nodes").await, nodes + 1);
        assert_eq!(count(&state, "runs").await, runs + 1);
    }

    #[tokio::test]
    async fn branch_start_refuses_a_finished_job_and_an_unknown_caller_node() {
        let (state, app, caller, _repo) = tree().await;
        let unknown = post(&app, &uri(), body("unknown-caller")).await;
        assert_eq!(unknown.0, StatusCode::NOT_FOUND, "{:?}", unknown.1);

        sqlx::query("UPDATE tree_jobs SET state = 'finished' WHERE id = ?")
            .bind(TREE)
            .execute(&state.pool)
            .await
            .unwrap();

        let finished = post(&app, &uri(), body(&caller)).await;
        assert_eq!(finished.0, StatusCode::CONFLICT, "{:?}", finished.1);
    }

    #[tokio::test]
    async fn branch_start_surfaces_an_admission_refusal_without_leaving_a_node() {
        let (state, app, caller, _repo) = tree().await;
        let mut job = crate::tree_job::store::load(&state.pool, TREE)
            .await
            .unwrap()
            .unwrap();
        job.manifest.costs_gb.insert("core".to_owned(), json!(1e20));
        crate::tree_job::store::update(&state.pool, &mut job)
            .await
            .unwrap();
        let before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM work_nodes")
            .fetch_one(&state.pool)
            .await
            .unwrap();

        let refused = post(
            &app,
            &uri(),
            json!({
                "name": "too-large",
                "goal": "Refused before a node is opened",
                "owned_paths": ["core/**"],
                "prompt": "Do not run.",
                "layer": "core",
                "node_id": caller,
            }),
        )
        .await;

        assert_eq!(refused.0, StatusCode::CONFLICT, "{:?}", refused.1);
        let after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM work_nodes")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(after, before);
    }
}
