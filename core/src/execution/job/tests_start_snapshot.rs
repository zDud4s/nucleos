//! M4 C5 tests: the graph a job starts with is resolved once, frozen on its row, and loaded back
//! from that row (never re-resolved).

// Each test holds `test_env_lock` across its awaits on purpose: it serialises tests that set
// process-wide env vars (current-thread runtimes, so no deadlock).
#![allow(clippy::await_holding_lock)]

use super::engine::load_graph;
use super::tests::{WorktreeRootEnv, test_pool, test_state, walkable_repo, with_home};
use super::*;

const GRAPH: &str = "nodes:\n  - {id: plan, type: agent, tool: claude, model: opus}\n  - {id: review, type: agent}\nedges:\n  - {from: plan, to: review}\n";
const EDITED_GRAPH: &str = "nodes:\n  - {id: plan, type: agent}\n  - {id: other, type: agent}\nedges:\n  - {from: plan, to: other}\n";

/// Pins `bundle` for the project `nucleos` in the settings store, as `workflows::install` would.
/// The caller holds the settings (`Home::new()`) for the whole test.
fn pin_in_store(bundle: &crate::workflows::Bundle) {
    let mut pins = crate::project::rules::pins("nucleos").unwrap();
    crate::workflows::apply_install(&mut pins, bundle).unwrap();
    crate::project::rules::testing::publish_pins("nucleos", &crate::workflows::render_pins(&pins));
}

/// A library holding `dev@1.0` with `graph`, pinned by the project `nucleos`. Returns the bundle
/// directory and the pinned bundle.
fn pin_dev(
    container: &tempfile::TempDir,
    state: &mut AppState,
    graph: &str,
) -> (std::path::PathBuf, crate::workflows::Bundle) {
    let library = container.path().join("workflows");
    let bundle_dir = library.join("dev").join("1.0");
    std::fs::create_dir_all(&bundle_dir).unwrap();
    std::fs::write(bundle_dir.join("bundle.yaml"), "owns: []\n").unwrap();
    std::fs::write(bundle_dir.join("graph.yaml"), graph).unwrap();
    let bundle = crate::workflows::read_bundle(&bundle_dir, "dev", "1.0")
        .unwrap()
        .unwrap();
    pin_in_store(&bundle);
    state.workflow_library = Some(library);
    (bundle_dir, bundle)
}

/// Gives `dev` a model override on `plan` and switches `review` off.
fn overlay_dev() {
    let mut pins = crate::project::rules::pins("nucleos").unwrap();
    let nodes = &mut pins.workflows[0].nodes;
    nodes.insert(
        "plan".to_string(),
        crate::workflows::NodeOverlay {
            model: Some("haiku".to_string()),
            ..Default::default()
        },
    );
    nodes.insert(
        "review".to_string(),
        crate::workflows::NodeOverlay {
            disabled: Some(true),
            ..Default::default()
        },
    );
    crate::project::rules::testing::publish_pins("nucleos", &crate::workflows::render_pins(&pins));
}

fn request<'a>(root: &'a str, workflow: Option<&'a str>) -> StartRequest<'a> {
    StartRequest {
        project_id: "nucleos",
        project_root: root,
        rule_name: None,
        prompt: "do it",
        max_items: 1,
        gate_each: true,
        review: false,
        gate_retries: 0,
        head_sha: None,
        max_rounds: None,
        budget_usd: None,
        team_id: None,
        workflow,
    }
}

#[tokio::test(flavor = "current_thread")]
async fn a_started_job_snapshots_the_overlay_painted_graph() {
    let _lock = crate::execution::worktree::test_env_lock();
    let (container, repo) = walkable_repo("nucleos-job-c5a-", "git --version");
    let root = tempfile::tempdir().unwrap();
    let _env = WorktreeRootEnv::set(root.path());
    let _settings = crate::project::rules::testing::Home::new();
    let pool = test_pool().await;
    let mut state = with_home(test_state(pool.clone()).await, &container);
    pin_dev(&container, &mut state, GRAPH);
    overlay_dev();
    let project_root = repo.to_string_lossy().into_owned();

    let JobStart::Started(job_id) = start(&state, &request(&project_root, Some("dev"))).await
    else {
        panic!("the job did not start");
    };

    let snapshot = load_graph_snapshot(&pool, job_id).await.unwrap().unwrap();
    let effective = effective_from_snapshot(&snapshot).unwrap();
    assert_eq!(
        effective.node("plan").unwrap().model.as_deref(),
        Some("haiku"),
        "the overlay is painted into the node"
    );
    assert!(effective.is_disabled("review"));
    assert!(
        effective.node("review").is_some(),
        "a disabled node stays in the graph"
    );
    assert_eq!(snapshot.bundle_ref.as_deref(), Some("dev@1.0"));
}

#[tokio::test(flavor = "current_thread")]
async fn bundle_hash_in_the_snapshot_is_the_pin_hash() {
    let _lock = crate::execution::worktree::test_env_lock();
    let (container, repo) = walkable_repo("nucleos-job-c5b-", "git --version");
    let root = tempfile::tempdir().unwrap();
    let _env = WorktreeRootEnv::set(root.path());
    let _settings = crate::project::rules::testing::Home::new();
    let pool = test_pool().await;
    let mut state = with_home(test_state(pool.clone()).await, &container);
    let (_dir, bundle) = pin_dev(&container, &mut state, GRAPH);
    let project_root = repo.to_string_lossy().into_owned();

    let JobStart::Started(job_id) = start(&state, &request(&project_root, Some("dev"))).await
    else {
        panic!("the job did not start");
    };

    let pinned = crate::project::rules::pins("nucleos").unwrap().workflows[0]
        .hash
        .clone();
    let snapshot = load_graph_snapshot(&pool, job_id).await.unwrap().unwrap();
    assert_eq!(snapshot.bundle_hash.as_deref(), Some(pinned.as_str()));
    assert_eq!(pinned, bundle.hash);
}

#[tokio::test(flavor = "current_thread")]
async fn editing_the_bundle_after_start_does_not_change_the_running_job() {
    let _lock = crate::execution::worktree::test_env_lock();
    let (container, repo) = walkable_repo("nucleos-job-c5c-", "git --version");
    let root = tempfile::tempdir().unwrap();
    let _env = WorktreeRootEnv::set(root.path());
    let _settings = crate::project::rules::testing::Home::new();
    let pool = test_pool().await;
    let mut state = with_home(test_state(pool.clone()).await, &container);
    let (bundle_dir, _bundle) = pin_dev(&container, &mut state, GRAPH);
    let project_root = repo.to_string_lossy().into_owned();

    let JobStart::Started(job_id) = start(&state, &request(&project_root, Some("dev"))).await
    else {
        panic!("the job did not start");
    };
    let before = load_graph(&pool, job_id).await.unwrap();

    // Edit the library graph and re-pin it (spec 12.3): a new bundle hash, a new graph.
    std::fs::write(bundle_dir.join("graph.yaml"), EDITED_GRAPH).unwrap();
    let edited = crate::workflows::read_bundle(&bundle_dir, "dev", "1.0")
        .unwrap()
        .unwrap();
    pin_in_store(&edited);

    let after = load_graph(&pool, job_id).await.unwrap();
    assert_eq!(
        after, before,
        "a running job keeps the graph it started with"
    );
    assert!(after.node("review").is_some());
    assert!(after.node("other").is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn no_pin_snapshots_the_embedded_autopilot_with_its_digest() {
    let _lock = crate::execution::worktree::test_env_lock();
    let (container, repo) = walkable_repo("nucleos-job-c5d-", "git --version");
    let root = tempfile::tempdir().unwrap();
    let _env = WorktreeRootEnv::set(root.path());
    let _settings = crate::project::rules::testing::Home::new();
    let pool = test_pool().await;
    let state = with_home(test_state(pool.clone()).await, &container);
    let project_root = repo.to_string_lossy().into_owned();

    let JobStart::Started(job_id) = start(&state, &request(&project_root, None)).await else {
        panic!("the job did not start");
    };

    let snapshot = load_graph_snapshot(&pool, job_id).await.unwrap().unwrap();
    assert_eq!(
        snapshot.bundle_ref,
        Some(format!("{}@{}", crate::seed::NAME, crate::seed::VERSION))
    );
    assert_eq!(snapshot.bundle_hash, Some(crate::seed::digest()));
    let embedded = crate::workflows::graph::parse(crate::seed::graph_yaml()).unwrap();
    assert_eq!(effective_from_snapshot(&snapshot).unwrap().graph, embedded);
}

#[tokio::test(flavor = "current_thread")]
async fn a_pin_with_an_unparsable_graph_refuses_the_start() {
    let _lock = crate::execution::worktree::test_env_lock();
    let (container, repo) = walkable_repo("nucleos-job-c5e-", "git --version");
    let root = tempfile::tempdir().unwrap();
    let _env = WorktreeRootEnv::set(root.path());
    let _settings = crate::project::rules::testing::Home::new();
    let pool = test_pool().await;
    let mut state = with_home(test_state(pool.clone()).await, &container);
    // Pinned AFTER the bad graph is written, so the pin's hash matches the library bundle.
    pin_dev(&container, &mut state, "nodes: [unterminated\n");
    let project_root = repo.to_string_lossy().into_owned();

    let started = start(&state, &request(&project_root, Some("dev"))).await;

    assert!(
        matches!(started, JobStart::NoGraph(_)),
        "a bad pinned graph must refuse, never fall back to the autopilot: {started:?}"
    );
    let jobs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM jobs")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(jobs, 0, "a refused start leaves no row behind");
}

#[tokio::test(flavor = "current_thread")]
async fn a_workflow_the_project_does_not_pin_refuses_the_start() {
    let _lock = crate::execution::worktree::test_env_lock();
    let (container, repo) = walkable_repo("nucleos-job-c5f-", "git --version");
    let root = tempfile::tempdir().unwrap();
    let _env = WorktreeRootEnv::set(root.path());
    let _settings = crate::project::rules::testing::Home::new();
    let pool = test_pool().await;
    let state = with_home(test_state(pool.clone()).await, &container);
    let project_root = repo.to_string_lossy().into_owned();

    let started = start(&state, &request(&project_root, Some("nope"))).await;

    assert!(matches!(started, JobStart::NoGraph(_)), "{started:?}");
}

#[tokio::test]
async fn a_null_snapshot_loads_as_the_embedded_autopilot() {
    let pool = test_pool().await;
    let job_id = insert_job(
        &pool,
        &NewJob {
            project_id: "project-a",
            project_root: "/p",
            rule_name: None,
            prompt: "old job",
            max_items: 1,
            gate_each: true,
            review: false,
            gate_retries: 0,
            head_sha: None,
            max_rounds: None,
            budget_usd: None,
            team_id: None,
            graph: None,
        },
    )
    .await
    .unwrap();

    let loaded = load_graph(&pool, job_id).await.unwrap();

    let embedded = crate::workflows::graph::parse(crate::seed::graph_yaml()).unwrap();
    assert_eq!(loaded.graph, embedded);
    assert!(loaded.disabled.is_empty());
}

#[cfg(test)]
mod pure {
    use super::*;

    #[test]
    fn no_state_and_no_library_resolve_to_the_embedded_autopilot() {
        let _home = crate::project::rules::testing::Home::new();
        let resolved = resolve_graph_from(
            None,
            "nucleos",
            std::path::Path::new("/p"),
            crate::seed::NAME,
        )
        .unwrap();
        assert!(resolved.builtin);
        assert_eq!(resolved.bundle_hash, Some(crate::seed::digest()));
    }

    #[test]
    fn another_workflow_with_no_pin_is_a_refusal_not_the_autopilot() {
        let _home = crate::project::rules::testing::Home::new();
        let refusal =
            resolve_graph_from(None, "nucleos", std::path::Path::new("/p"), "dev").unwrap_err();
        assert!(refusal.contains("dev"), "{refusal}");
    }

    /// RED (4.2): `resolve_graph_from` loses `machine_root` and finds the pin in the store by project id. Nothing
    /// is written to a pins file, so the resolution cannot lean on `is_file()`.
    #[test]
    fn resolve_graph_from_sees_a_stored_pin() {
        let _home = crate::project::rules::testing::Home::new();
        let container = tempfile::tempdir().unwrap();
        let library = container.path().join("workflows");
        let bundle_dir = library.join("dev").join("1.0");
        std::fs::create_dir_all(&bundle_dir).unwrap();
        std::fs::write(bundle_dir.join("bundle.yaml"), "owns: []\n").unwrap();
        std::fs::write(bundle_dir.join("graph.yaml"), GRAPH).unwrap();
        let bundle = crate::workflows::read_bundle(&bundle_dir, "dev", "1.0")
            .unwrap()
            .unwrap();
        let project_id = "resolve-stored-pin-project";

        // Not pinned yet: a workflow other than the autopilot is a refusal.
        let refusal = resolve_graph_from(
            Some(library.as_path()),
            project_id,
            std::path::Path::new("/p"),
            "dev",
        )
        .unwrap_err();
        assert!(refusal.contains("dev"), "{refusal}");

        crate::project::rules::testing::publish_pins(
            project_id,
            &format!(
                "workflows:\n  - name: dev\n    version: '1.0'\n    origin: library:dev@1.0\n    hash: {}\n",
                bundle.hash
            ),
        );
        let resolved = resolve_graph_from(
            Some(library.as_path()),
            project_id,
            std::path::Path::new("/p"),
            "dev",
        )
        .unwrap();
        assert!(!resolved.builtin);
        assert_eq!(resolved.bundle_ref.as_deref(), Some("dev@1.0"));
        assert_eq!(resolved.bundle_hash.as_deref(), Some(bundle.hash.as_str()));
    }
}
