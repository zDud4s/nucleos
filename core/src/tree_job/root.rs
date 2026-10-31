//! Tree-job root node handling: the top-level node of a job (scaffold placeholder).

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use serde_json::json;
    use tower::ServiceExt;

    use crate::execution::git_exec::testkit::{initialize_repo, space_free_tempdir};
    use crate::tree_job::http::routes;

    const GOAL: &str = "Ship the unmistakable goal";

    /// The prompt of the only run `POST /trees` started. It goes through the route, so that the test
    /// names no signature GREEN may still choose.
    #[tokio::test]
    async fn the_root_run_prompt_carries_the_goal_and_the_protocol() {
        let container = space_free_tempdir("tree-root-");
        let repo = container.path().join("repo");
        initialize_repo(&repo);
        let git = |args: &[&str]| {
            let output = std::process::Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(args)
                .output()
                .expect("git should start");
            assert!(output.status.success(), "git {args:?} failed");
        };
        git(&["branch", "-M", "master"]);
        let workflow = repo.join(".nucleos/workflow");
        std::fs::create_dir_all(&workflow).unwrap();
        std::fs::write(workflow.join("speed.yaml"), "tree:\n  floor_gb: 0\n").unwrap();
        let (state, runner) = crate::execution::runs::testkit::test_state_with_runner(
            Some(Duration::from_secs(5)),
            Duration::from_secs(600),
        )
        .await;
        runner.canned.lock().unwrap().as_mut().unwrap().session_id = None;
        let body = json!({
            "id": "job-1",
            "goal": GOAL,
            "feature_branch": "feat/tree",
            "project_root": repo.to_string_lossy().replace('\\', "/"),
            "project_id": "project-0",
        });
        let request = Request::builder()
            .method("POST")
            .uri("/trees")
            .header("Authorization", "Bearer test-token")
            .header("Content-Type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();

        let response = routes().with_state(state.clone()).oneshot(request).await.unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let prompts: Vec<String> = sqlx::query_scalar("SELECT prompt FROM runs")
            .fetch_all(&state.pool)
            .await
            .unwrap();
        assert_eq!(prompts.len(), 1, "expected exactly one root run: {prompts:?}");
        let prompt = &prompts[0];
        assert!(prompt.contains(GOAL), "the goal is missing: {prompt}");
        // The protocol names the tools the root uses and the way it ends.
        for needle in ["tree_branch_start", "node_close", "handoff"] {
            assert!(prompt.contains(needle), "the protocol lacks {needle:?}: {prompt}");
        }
    }
}
