//! End-to-end tests of adaptive depth: real git repository, real worktrees, the real
//! `branch_start` and `GET /trees/{id}` routes and the driver with a scripted gate. The agents are
//! played by the tests (they commit in the worktree and report), so what is under test is the
//! daemon's side: the shape a start gives the tree, the order of the merges, and the gates.
//!
//! The fixtures are the driver tests' (`driver::tests`).

// The process-wide env guard is held across awaits on purpose, as in the driver tests.
#![allow(clippy::await_holding_lock)]

use std::path::Path;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use tower::ServiceExt;

use super::driver::GateVerdict;
use super::driver::tests::{FakeGate, Fixture, commit_file, git_in};
use super::model::JOB_TERMINAL_STATE;
use super::{http, nesting, open};
use crate::auth::Scope;
use crate::execution::git_exec::GitExecutor;
use crate::execution::git_exec::tests::sha_of;
use crate::execution::vcs::drain_once;

/// `speed.yaml` of the test repository: `master` as the integration branch (the main checkout
/// stands on the feature branch, which is what an unset base would resolve to), room for many live
/// branches, no disk floor, and the depth limit under test.
fn speed(max_depth: i64) -> String {
    format!(
        "tree:\n  base_branch: master\n  floor_gb: 0\n  max_live_branches: 12\nnodes:\n  \
         max_depth: {max_depth}\n  leaf_budget: 120000\n"
    )
}

/// A gate that answers green for as long as a test needs.
fn green_gate() -> FakeGate {
    FakeGate::answering((0..40).map(|_| GateVerdict::Green).collect())
}

/// One tree job over a real repository, and the means to start its branches the way the
/// `tree_branch_start` tool does.
struct Tree {
    fx: Fixture,
    job: &'static str,
    app: Router,
    root: String,
}

impl Tree {
    async fn new(job: &'static str, max_depth: i64) -> Tree {
        let fx = Fixture::new().await;
        let workflow = fx.repo.join(".nucleos/workflow");
        std::fs::create_dir_all(&workflow).unwrap();
        commit_file(
            &fx.repo,
            ".nucleos/workflow/speed.yaml",
            &speed(max_depth),
            "speed",
        );
        // The branch worktrees live under the project root; the main checkout must not see them.
        let exclude = fx.repo.join(".git/info/exclude");
        std::fs::create_dir_all(exclude.parent().unwrap()).unwrap();
        let mut text = std::fs::read_to_string(&exclude).unwrap_or_default();
        text.push_str("\n/.claude/worktrees/\n");
        std::fs::write(&exclude, text).unwrap();
        fx.seed_job(job, vec![], None).await;
        let app = http::routes().with_state(fx.state.clone());
        let root = super::driver::tests::root_node_of(job);
        Tree {
            fx,
            job,
            app,
            root,
        }
    }

    async fn send(&self, request: Request<Body>) -> (StatusCode, Value) {
        let mut request = request;
        request.extensions_mut().insert(Scope::Control);
        let response = self.app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
    }

    /// `tree_branch_start` as `caller`, a leaf-or-orchestrator `kind` and an estimate.
    async fn start(
        &self,
        caller: &str,
        name: &str,
        paths: &[&str],
        kind: Option<&str>,
        estimate: Option<i64>,
    ) -> (StatusCode, Value) {
        let mut body = json!({
            "name": name,
            "goal": format!("Build {name}"),
            "owned_paths": paths,
            "prompt": format!("Build {name}."),
            "layer": "core",
            "node_id": caller,
        });
        if let Some(kind) = kind {
            body["kind"] = json!(kind);
        }
        if let Some(estimate) = estimate {
            body["estimate_tokens"] = json!(estimate);
        }
        let request = Request::builder()
            .method("POST")
            .uri(format!("/trees/{}/branch_start", self.job))
            .header("Content-Type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        self.send(request).await
    }

    /// A start that has to succeed; the answer.
    async fn started(
        &self,
        caller: &str,
        name: &str,
        paths: &[&str],
        kind: Option<&str>,
        estimate: Option<i64>,
    ) -> Value {
        let (status, answer) = self.start(caller, name, paths, kind, estimate).await;
        assert_eq!(status, StatusCode::OK, "start of {name}: {answer:?}");
        answer
    }

    /// `GET /trees/{id}`.
    async fn status(&self) -> Value {
        let request = Request::builder()
            .method("GET")
            .uri(format!("/trees/{}", self.job))
            .body(Body::empty())
            .unwrap();
        let (status, body) = self.send(request).await;
        assert_eq!(status, StatusCode::OK, "{body:?}");
        body
    }

    /// The git branch of a tree branch.
    fn git_branch(&self, name: &str) -> String {
        format!("tree/{}/{name}", self.job)
    }

    fn tip(&self, name: &str) -> String {
        sha_of(&self.fx.repo, &self.git_branch(name))
    }

    /// The branch's agent commits `file` in its worktree.
    fn commits(started: &Value, file: &str) {
        let worktree = Path::new(started["worktree"].as_str().expect("a worktree"));
        if let Some(dir) = Path::new(file).parent() {
            std::fs::create_dir_all(worktree.join(dir)).unwrap();
        }
        commit_file(worktree, file, "work\n", file);
    }

    /// The branch's node ends its run with `result`.
    async fn reports(&self, started: &Value, result: &str) {
        let node = started["node_id"].as_str().expect("a node");
        self.fx.run_ended(node, Some(result)).await;
    }

    /// A leaf that commits `file` and reports done.
    async fn leaf_works(&self, started: &Value, file: &str) {
        Self::commits(started, file);
        self.reports(started, "done").await;
    }

    /// One pass of the driver, the gates it spawned, and the merge queue.
    async fn round(&self, gate: &FakeGate) {
        self.fx.tick(self.job, gate).await;
        // The gate task records its verdict on its own; give it the lock.
        tokio::time::sleep(Duration::from_millis(40)).await;
        while drain_once(&self.fx.state.pool, &self.fx.key, &GitExecutor::default()).await {}
    }

    fn summary(job: &super::store::TreeJob) -> String {
        job.manifest
            .branches
            .values()
            .map(|branch| format!("{}:{}", branch.name, branch.state))
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// Rounds until the job is finished (landable and held), or fails with where it stuck.
    async fn drive(&self, gate: &FakeGate) {
        for _ in 0..80 {
            self.round(gate).await;
            if self.fx.job(self.job).await.state == JOB_TERMINAL_STATE {
                return;
            }
        }
        let job = self.fx.job(self.job).await;
        panic!("job {} never finished: {}", self.job, Self::summary(&job));
    }

    /// The calls the gate got, as branch names, in order.
    fn gated(gate: &FakeGate) -> Vec<String> {
        gate.calls().into_iter().map(|(_, branch)| branch).collect()
    }

    fn feature_has(&self, file: &str) -> bool {
        git_in(
            &self.fx.repo,
            &["cat-file", "-e", &format!("feat/tree:{file}")],
        )
    }

    fn reachable(&self, sha: &str, git_ref: &str) -> bool {
        git_in(
            &self.fx.repo,
            &["merge-base", "--is-ancestor", sha, git_ref],
        )
    }

    async fn merge_sha(&self, name: &str) -> String {
        let branch = self.fx.branch(self.job, name).await;
        assert_eq!(branch.state, "merged", "branch {name}");
        branch.extra["merge_sha"].as_str().expect("a merge sha").to_owned()
    }

    async fn descents(&self) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM feed WHERE kind = 'work_tree_descent'")
            .fetch_one(&self.fx.state.pool)
            .await
            .unwrap()
    }

    async fn node_count(&self) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM work_nodes")
            .fetch_one(&self.fx.state.pool)
            .await
            .unwrap()
    }
}

#[tokio::test]
async fn a_small_task_is_a_two_level_tree_identical_to_today() {
    let tree = Tree::new("e2e-flat", 6).await;
    let a = tree.started(&tree.root, "a", &["a/**"], None, None).await;
    let b = tree
        .started(&tree.root, "b", &["b/**"], Some("leaf"), Some(50_000))
        .await;
    for answer in [&a, &b] {
        assert_eq!(answer["kind"], "leaf");
        assert_eq!(answer["depth"], 1);
        assert_eq!(answer["parent"], Value::Null);
        assert_eq!(answer["degraded"], Value::Null);
    }
    // The prompt of a stem-level leaf is the task, with no orchestrator text in front of it.
    let prompt = tree.fx.prompt_of(a["run_id"].as_i64().unwrap()).await;
    assert!(prompt.contains("Build a."), "{prompt:?}");
    assert!(!prompt.contains("You are an orchestrator at depth"), "{prompt:?}");
    tree.leaf_works(&a, "a/one.txt").await;
    tree.leaf_works(&b, "b/one.txt").await;
    let (tip_a, tip_b) = (tree.tip("a"), tree.tip("b"));
    let master = sha_of(&tree.fx.repo, "master");
    tree.fx.root_done(tree.job).await;
    let gate = green_gate();

    tree.drive(&gate).await;

    let job = tree.fx.job(tree.job).await;
    for name in ["a", "b"] {
        let branch = &job.manifest.branches[name];
        assert_eq!(branch.parent, None, "{name}");
        assert_eq!(branch.base, None, "{name}");
        assert!(!branch.extra.contains_key("descent"), "{name}");
        assert!(!branch.extra.contains_key("degraded"), "{name}");
        assert!(nesting::children(&job.manifest, name).is_empty());
    }
    // Both merged straight into the feature branch, which is the only ref that moved.
    for (name, tip) in [("a", tip_a), ("b", tip_b)] {
        let sha = tree.merge_sha(name).await;
        assert_eq!(sha_of(&tree.fx.repo, &format!("{sha}^2")), tip, "{name}");
        assert!(tree.reachable(&sha, "feat/tree"), "{name}");
    }
    assert!(tree.feature_has("a/one.txt") && tree.feature_has("b/one.txt"));
    assert_eq!(sha_of(&tree.fx.repo, "master"), master);
    assert_eq!(tree.descents().await, 0, "a leaf start records no descent");
    let status = tree.status().await;
    let nodes = status["nodes"].as_array().unwrap();
    assert_eq!(nodes.len(), 2);
    assert!(nodes.iter().all(|node| node["depth"] == 1 && node["kind"] == "leaf"));
    assert_eq!(status["start_refusals"], json!([]));
    assert_eq!(status["state"], JOB_TERMINAL_STATE);
}

#[tokio::test]
async fn one_oversized_piece_becomes_an_orchestrator_whose_leaves_merge_into_it() {
    let tree = Tree::new("e2e-orch", 6).await;
    let a = tree
        .started(&tree.root, "a", &["a/**"], Some("leaf"), Some(40_000))
        .await;
    let b = tree
        .started(&tree.root, "b", &["b/**"], Some("orchestrator"), Some(180_000))
        .await;
    assert_eq!(b["kind"], "orchestrator");
    assert_eq!(b["parent"], Value::Null, "the stem is not nested");
    let b_node = b["node_id"].as_str().unwrap().to_owned();
    let b1 = tree
        .started(&b_node, "b-1", &["b/one/**"], Some("leaf"), Some(60_000))
        .await;
    let b2 = tree
        .started(&b_node, "b-2", &["b/two/**"], Some("leaf"), Some(60_000))
        .await;
    for (answer, parent) in [(&b1, "b"), (&b2, "b")] {
        assert_eq!(answer["parent"], parent);
        assert_eq!(answer["depth"], 2);
        assert_eq!(answer["kind"], "leaf");
    }
    // The children start from the parent's tip, and the orchestrator edits nothing itself.
    tree.leaf_works(&a, "a/one.txt").await;
    tree.leaf_works(&b1, "b/one/x.txt").await;
    tree.leaf_works(&b2, "b/two/x.txt").await;
    tree.reports(&b, "done").await;
    tree.fx.root_done(tree.job).await;
    let gate = green_gate();

    tree.drive(&gate).await;

    let job = tree.fx.job(tree.job).await;
    assert!(
        nesting::children(&job.manifest, "a").is_empty(),
        "a never descends"
    );
    assert!(!job.manifest.branches["a"].extra.contains_key("descent"));
    assert_eq!(job.manifest.branches["b"].extra["descent"]["kind"], "orchestrator");
    assert_eq!(nesting::children(&job.manifest, "b").len(), 2);
    // The leaves merged into b's git branch, and b merged into the feature branch.
    let b_ref = tree.git_branch("b");
    for name in ["b-1", "b-2"] {
        let sha = tree.merge_sha(name).await;
        assert_eq!(sha_of(&tree.fx.repo, &format!("{sha}^2")), tree.tip(name));
        assert!(tree.reachable(&sha, &b_ref), "{name} merged into b");
    }
    let b_sha = tree.merge_sha("b").await;
    assert_eq!(sha_of(&tree.fx.repo, &format!("{b_sha}^2")), tree.tip("b"));
    assert!(tree.reachable(&b_sha, "feat/tree"));
    let a_sha = tree.merge_sha("a").await;
    assert!(!tree.reachable(&a_sha, &b_ref), "a merged into the feature, not into b");
    for file in ["a/one.txt", "b/one/x.txt", "b/two/x.txt"] {
        assert!(tree.feature_has(file), "{file}");
    }
    // b is gated only after both leaves merged into it.
    let gated = Tree::gated(&gate);
    let position = |name: &str| gated.iter().position(|gated| gated == name).unwrap();
    assert!(position("b-1") < position("b") && position("b-2") < position("b"), "{gated:?}");
    assert_eq!(gated.len(), 4, "{gated:?}");
    assert_eq!(tree.descents().await, 1, "only the orchestrator records a descent");
}

#[tokio::test]
async fn a_three_level_descent_closes_end_to_end_without_basenotmerged_or_deadlock() {
    let tree = Tree::new("e2e-deep", 6).await;
    let b = tree
        .started(&tree.root, "b", &["b/**"], Some("orchestrator"), Some(900_000))
        .await;
    let b_node = b["node_id"].as_str().unwrap().to_owned();
    let c = tree
        .started(&b_node, "b-c", &["b/c/**"], Some("orchestrator"), Some(400_000))
        .await;
    let d = tree
        .started(&b_node, "b-d", &["b/d/**"], Some("leaf"), Some(60_000))
        .await;
    let c_node = c["node_id"].as_str().unwrap().to_owned();
    assert_eq!((c["depth"].as_i64(), c["parent"].as_str()), (Some(2), Some("b")));
    let c1 = tree
        .started(&c_node, "b-c-1", &["b/c/one/**"], Some("leaf"), Some(60_000))
        .await;
    let c2 = tree
        .started(&c_node, "b-c-2", &["b/c/two/**"], Some("leaf"), Some(60_000))
        .await;
    for leaf in [&c1, &c2] {
        assert_eq!((leaf["depth"].as_i64(), leaf["parent"].as_str()), (Some(3), Some("b-c")));
    }
    tree.leaf_works(&c1, "b/c/one/x.txt").await;
    tree.leaf_works(&c2, "b/c/two/x.txt").await;
    tree.leaf_works(&d, "b/d/x.txt").await;
    tree.reports(&c, "done").await;
    tree.reports(&b, "done").await;
    tree.fx.root_done(tree.job).await;
    let gate = green_gate();

    tree.drive(&gate).await;

    let job = tree.fx.job(tree.job).await;
    for (name, parent) in [("b-c", "b"), ("b-d", "b"), ("b-c-1", "b-c"), ("b-c-2", "b-c")] {
        assert_eq!(job.manifest.branches[name].parent.as_deref(), Some(parent));
        let sha = tree.merge_sha(name).await;
        assert!(tree.reachable(&sha, &tree.git_branch(parent)), "{name} into {parent}");
    }
    assert_eq!(job.manifest.branches["b"].parent, None);
    let b_sha = tree.merge_sha("b").await;
    assert!(tree.reachable(&b_sha, "feat/tree"));
    for file in ["b/c/one/x.txt", "b/c/two/x.txt", "b/d/x.txt"] {
        assert!(tree.feature_has(file), "{file}");
    }
    // Nobody was parked and no branch waited on a base: every question and refusal stays empty.
    let questions: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM decisions")
        .fetch_one(&tree.fx.state.pool)
        .await
        .unwrap();
    assert_eq!(questions, 0);
    assert!(job.manifest.extra.get("pending_starts").is_none_or(|p| p == &json!({})));
    assert_eq!(tree.status().await["start_refusals"], json!([]));
    // Each parent was gated after every one of its children had merged.
    let gated = Tree::gated(&gate);
    let position = |name: &str| gated.iter().position(|gated| gated == name).unwrap();
    for (parent, children) in [("b-c", &["b-c-1", "b-c-2"][..]), ("b", &["b-c", "b-d"][..])] {
        for child in children {
            assert!(position(child) < position(parent), "{child} before {parent}: {gated:?}");
        }
    }
    assert_eq!(gated.len(), 5, "{gated:?}");
}

#[tokio::test]
async fn a_failed_child_runs_the_existing_rescue_and_does_not_leave_the_parent_stuck() {
    let tree = Tree::new("e2e-rescue", 6).await;
    let b = tree
        .started(&tree.root, "b", &["b/**"], Some("orchestrator"), Some(300_000))
        .await;
    let b_node = b["node_id"].as_str().unwrap().to_owned();
    let b1 = tree
        .started(&b_node, "b-1", &["b/one/**"], Some("leaf"), Some(60_000))
        .await;
    let b2 = tree
        .started(&b_node, "b-2", &["b/two/**"], Some("leaf"), Some(60_000))
        .await;
    let b2_node = b2["node_id"].as_str().unwrap().to_owned();
    tree.leaf_works(&b1, "b/one/x.txt").await;
    tree.reports(&b, "done").await;
    tree.reports(&b2, "failed").await;
    tree.fx.root_done(tree.job).await;
    let gate = green_gate();

    // The failed child gets its one rescue run, as any branch does.
    tree.round(&gate).await;
    assert_eq!(tree.fx.launches(&b2_node, "rescue-").await, 1);
    assert_eq!(tree.fx.branch(tree.job, "b-2").await.state, "building");

    // The rescue fails too: the owner is asked about the child, and the parent waits for it.
    tree.fx.relaunched_run_reports(&b2_node, "failed").await;
    for _ in 0..3 {
        tree.round(&gate).await;
    }
    assert_eq!(tree.fx.branch(tree.job, "b-2").await.state, "parked");
    assert_eq!(tree.fx.open_questions(tree.job, &b2_node).await.len(), 1);
    assert_eq!(tree.fx.branch(tree.job, "b-1").await.state, "merged");
    assert_eq!(tree.fx.branch(tree.job, "b").await.state, "building");
    assert!(!Tree::gated(&gate).contains(&"b".to_owned()), "{:?}", Tree::gated(&gate));
    assert_eq!(tree.fx.job(tree.job).await.state, "open");

    // The owner answers: the child's new run finishes and the parent follows it to the feature.
    let decision: String = sqlx::query_scalar("SELECT id FROM decisions WHERE node_id = ?")
        .bind(&b2_node)
        .fetch_one(&tree.fx.state.pool)
        .await
        .unwrap();
    open::answer_and_relaunch(&tree.fx.state, &decision, "try again", "shell")
        .await
        .unwrap();
    Tree::commits(&b2, "b/two/x.txt");
    tree.fx.relaunched_run_reports_done(&b2_node).await;
    tree.drive(&gate).await;

    for name in ["b-1", "b-2", "b"] {
        assert_eq!(tree.fx.branch(tree.job, name).await.state, "merged", "{name}");
    }
    assert!(tree.feature_has("b/one/x.txt") && tree.feature_has("b/two/x.txt"));
    let gated = Tree::gated(&gate);
    assert_eq!(gated.last().map(String::as_str), Some("b"), "{gated:?}");
}

#[tokio::test]
async fn reaching_max_depth_degrades_to_a_leaf_instead_of_failing_the_tree() {
    let tree = Tree::new("e2e-limit", 3).await;
    let b = tree
        .started(&tree.root, "b", &["b/**"], Some("orchestrator"), Some(900_000))
        .await;
    let c = tree
        .started(
            b["node_id"].as_str().unwrap(),
            "b-c",
            &["b/c/**"],
            Some("orchestrator"),
            Some(900_000),
        )
        .await;
    assert_eq!((c["kind"].as_str(), c["depth"].as_i64()), (Some("orchestrator"), Some(2)));
    let c_node = c["node_id"].as_str().unwrap().to_owned();

    // Depth 3 is the last level: an orchestrator is asked for and a leaf is opened.
    let d = tree
        .started(&c_node, "b-c-d", &["b/c/d/**"], Some("orchestrator"), Some(900_000))
        .await;
    assert_eq!((d["kind"].as_str(), d["depth"].as_i64()), (Some("leaf"), Some(3)));
    assert!(d["degraded"].as_str().unwrap().contains("depth 3"), "{d:?}");
    let job = tree.fx.job(tree.job).await;
    let recorded = &job.manifest.branches["b-c-d"];
    assert!(recorded.extra["degraded"].as_str().unwrap().contains("depth 3"));
    assert_eq!(recorded.extra["descent"]["kind"], "leaf");
    assert_eq!(job.state, "open", "the tree is not failed");

    // Nothing opens below it.
    let nodes = tree.node_count().await;
    let (status, refused) = tree
        .start(
            d["node_id"].as_str().unwrap(),
            "too-deep",
            &["b/c/d/x/**"],
            None,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{refused:?}");
    assert!(refused.to_string().contains("depth 4"), "{refused:?}");
    assert_eq!(tree.node_count().await, nodes);

    // The status says what happened, and the tree still closes.
    let status = tree.status().await;
    let entry = status["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|node| node["branch"] == "b-c-d")
        .expect("the leaf is in the status");
    assert_eq!((entry["kind"].as_str(), entry["depth"].as_i64()), (Some("leaf"), Some(3)));
    assert!(entry["degraded"].as_str().unwrap().contains("depth 3"));
    let refusals = status["start_refusals"].as_array().unwrap();
    assert_eq!(refusals.last().unwrap()["branch"], "too-deep");
    tree.leaf_works(&d, "b/c/d/x.txt").await;
    tree.reports(&c, "done").await;
    tree.reports(&b, "done").await;
    tree.fx.root_done(tree.job).await;
    let gate = green_gate();
    tree.drive(&gate).await;
    assert!(tree.feature_has("b/c/d/x.txt"));
}

#[tokio::test]
async fn sibling_leaves_under_one_parent_cannot_share_files() {
    let tree = Tree::new("e2e-overlap", 6).await;
    let b = tree
        .started(&tree.root, "b", &["b/**"], Some("orchestrator"), Some(300_000))
        .await;
    let b_node = b["node_id"].as_str().unwrap().to_owned();
    tree.started(&b_node, "b-1", &["b/x/**"], Some("leaf"), Some(50_000))
        .await;
    let nodes = tree.node_count().await;

    let (status, refused) = tree
        .start(&b_node, "b-2", &["b/x/y/**"], Some("leaf"), Some(50_000))
        .await;

    assert_eq!(status, StatusCode::CONFLICT, "{refused:?}");
    assert!(refused.to_string().contains("overlap"), "{refused:?}");
    assert_eq!(tree.node_count().await, nodes, "a refused sibling opens no node");
    let job = tree.fx.job(tree.job).await;
    assert!(!job.manifest.branches.contains_key("b-2"));
    let status = tree.status().await;
    assert_eq!(status["start_refusals"].as_array().unwrap().last().unwrap()["status"], 409);
    // A sibling of its own files is fine, and so is the parent's scope being wider than theirs.
    tree.started(&b_node, "b-3", &["b/z/**"], Some("leaf"), Some(50_000))
        .await;
}
