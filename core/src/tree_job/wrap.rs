//! Tree-job node wrap-up: ends a node run that already reported, and starts a branch node again
//! when its run ended with a `handoff` report or died without one.
//!
//! The driver owns every other outcome: a node whose run is gone after a `done`, `blocked` or
//! `failed` report is left alone here.

use crate::conductor::continuation::with_handoff;
use crate::conductor::sweep::CONTINUATION_STOPPED;
use crate::execution::runs::finalize_termination;
use crate::state::AppState;

use super::decisions;
use super::engine::branch_node_may_close;
use super::model::JOB_TERMINAL_STATE;
use super::open::{DIED_PREFIX, WRAP_PREFIX, latest_launch, relaunch_node, run_is_live};
use super::store;

/// Nodes one tick acts on, oldest first; nodes it leaves alone do not count.
const MAX_NODES_PER_TICK: usize = 20;
/// Caps of the relaunches that follow a `handoff` report (key prefix `WRAP_PREFIX`) and a run that
/// died without a report (`DIED_PREFIX`).
const WRAP_CAP: i64 = 3;
const DIED_CAP: i64 = 2;
const PARKED: &str = "parked";

type NodeRow = (String, String, Option<String>, Option<String>);

/// One pass over the running nodes of unfinished jobs. A failure on one node is logged and never
/// stops the others. Safe to call again at any time: every relaunch is keyed by the run it follows.
pub async fn tick(state: &AppState) {
    let rows: Result<Vec<NodeRow>, _> = sqlx::query_as(
        "SELECT n.id, n.tree_job_id, n.run_id, n.handoff_path \
         FROM work_nodes n JOIN tree_jobs j ON j.id = n.tree_job_id \
         WHERE n.status = 'running' AND j.state <> ? \
         ORDER BY n.created_at, n.id",
    )
    .bind(JOB_TERMINAL_STATE)
    .fetch_all(&state.pool)
    .await;
    let rows = match rows {
        Ok(rows) => rows,
        Err(error) => {
            tracing::warn!(%error, "tree wrap-up could not list the running nodes");
            return;
        }
    };
    let mut handled = 0;
    for (node_id, job_id, run_id, handoff) in rows {
        if handled >= MAX_NODES_PER_TICK {
            break;
        }
        match wrap_node(state, &node_id, &job_id, run_id, handoff).await {
            Ok(false) => {}
            Ok(true) => handled += 1,
            Err(error) => {
                handled += 1;
                tracing::warn!(node = %node_id, %error, "tree wrap-up failed for the node");
            }
        }
    }
}

/// Wraps one node up. `Ok(true)` when it acted (ended a run, relaunched, or stopped the chain).
async fn wrap_node(
    state: &AppState,
    node_id: &str,
    job_id: &str,
    run_id: Option<String>,
    handoff: Option<String>,
) -> Result<bool, String> {
    let pool = &state.pool;
    let Some(job) = store::load(pool, job_id).await.map_err(|e| e.to_string())? else {
        return Ok(false);
    };
    if job.state == JOB_TERMINAL_STATE {
        return Ok(false);
    }
    let branch = job
        .manifest
        .branches
        .values()
        .find(|branch| branch.node_id.as_deref() == Some(node_id));
    match branch {
        // Only an answer starts a parked branch again; a merged or dropped one is the driver's.
        Some(branch) if branch.state == PARKED || branch_node_may_close(branch) => {
            return Ok(false);
        }
        Some(_) => {}
        // A leaf child fails through `reconcile_running`; only the root is wrapped without a branch.
        None if job.root_node_id != node_id => return Ok(false),
        None => {}
    }
    let has_branch = branch.is_some();

    let waiting: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM decisions WHERE node_id = ? AND status = ?")
            .bind(node_id)
            .bind(decisions::STATUS_OPEN)
            .fetch_one(pool)
            .await
            .map_err(|e| e.to_string())?;
    if waiting > 0 {
        return Ok(false);
    }

    let Some(run_id) = run_id else {
        // A root whose start failed after its launch was stored: start it again under that key.
        if has_branch {
            return Ok(false);
        }
        let stored = latest_launch(pool, node_id).await.map_err(|e| e.to_string())?;
        if stored.is_none() {
            return Ok(false);
        }
        relaunch_node(state, node_id, "none", &format!("root-{node_id}"), None).await?;
        return Ok(true);
    };

    let live = run_is_live(pool, &run_id).await.map_err(|e| e.to_string())?;
    let report: Option<String> = sqlx::query_scalar(
        "SELECT result FROM tree_transitions WHERE node_id = ? AND transition = 'report' AND key = ?",
    )
    .bind(node_id)
    .bind(&run_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| e.to_string())?;

    match (live, report.as_deref()) {
        (true, Some("handoff" | "done" | "blocked" | "failed")) => {
            // It reported, so it has nothing left to do; its process may still be running.
            let Ok(number) = run_id.parse::<i64>() else {
                return Ok(false);
            };
            finalize_termination(state, number, "cancelled").await;
            Ok(true)
        }
        (true, _) => Ok(false),
        (false, Some("handoff")) => {
            let suffix = handoff.as_deref().map(|path| with_handoff("", path));
            relaunch_or_stop(
                state, &job.id, node_id, has_branch, &run_id, WRAP_PREFIX, WRAP_CAP, suffix,
            )
            .await
        }
        (false, Some(_)) => Ok(false),
        (false, None) => {
            relaunch_or_stop(
                state, &job.id, node_id, has_branch, &run_id, DIED_PREFIX, DIED_CAP, None,
            )
            .await
        }
    }
}

/// Starts the node again under `<prefix><dead run>`, unless `cap` links with that prefix already
/// exist: then it writes the stop line once and parks the branch.
#[allow(clippy::too_many_arguments)]
async fn relaunch_or_stop(
    state: &AppState,
    job_id: &str,
    node_id: &str,
    has_branch: bool,
    dead_run: &str,
    prefix: &str,
    cap: i64,
    suffix: Option<String>,
) -> Result<bool, String> {
    let pool = &state.pool;
    let key = format!("{prefix}{dead_run}");
    // The key being replayed is not a prior link.
    let prior: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM tree_transitions \
         WHERE node_id = ? AND transition = 'launch' AND key LIKE ? AND key <> ?",
    )
    .bind(node_id)
    .bind(format!("{prefix}%"))
    .bind(&key)
    .fetch_one(pool)
    .await
    .map_err(|e| e.to_string())?;
    if prior < cap {
        relaunch_node(state, node_id, dead_run, &key, suffix.as_deref()).await?;
        return Ok(true);
    }

    let subject = format!("node:{node_id}");
    let seen: Option<i64> =
        sqlx::query_scalar("SELECT id FROM feed WHERE kind = ? AND subject = ? LIMIT 1")
            .bind(CONTINUATION_STOPPED)
            .bind(&subject)
            .fetch_optional(pool)
            .await
            .map_err(|e| e.to_string())?;
    if seen.is_none() {
        sqlx::query(
            "INSERT INTO feed (kind, summary, run_id, subject, created_at) VALUES (?, ?, NULL, ?, ?)",
        )
        .bind(CONTINUATION_STOPPED)
        .bind("Node ended too many times in a row: its chain stopped.")
        .bind(&subject)
        .bind(chrono::Utc::now().to_rfc3339())
        .execute(pool)
        .await
        .map_err(|e| e.to_string())?;
    }
    if has_branch {
        // `park` takes the job's lock itself, so none is held here.
        let question = format!(
            "Node {node_id} ended {cap} times in a row without finishing; how should the branch go on?"
        );
        decisions::park(pool, job_id, node_id, &question)
            .await
            .map_err(|e| e.to_string())?;
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::tree_job::model::{Branch, Manifest};
    use crate::tree_job::node_run;
    use crate::tree_job::open::{self, StoredLaunch};
    use crate::tree_job::store::{self, TreeJob};

    const JOB: &str = "wrap-job";
    const NODE: &str = "wrap-node";
    const CREATED: &str = "2026-10-31T00:00:00+00:00";

    async fn state() -> AppState {
        let (state, runner) = crate::execution::runs::testkit::test_state_with_runner(
            Some(Duration::from_secs(60)),
            Duration::from_secs(600),
        )
        .await;
        runner.canned.lock().unwrap().as_mut().unwrap().session_id = None;
        state
    }

    async fn seed_node(
        state: &AppState,
        branch_state: Option<&str>,
        job_state: &str,
        node_id: &str,
        run_id: Option<i64>,
    ) {
        let mut manifest = Manifest::new(JOB, "feat/wrap", "Wrap the run", "C:/nowhere", &[]);
        manifest.state = job_state.to_owned();
        if let Some(branch_state) = branch_state {
            let mut branch = Branch::new("branch", &["core/src/**".to_owned()], "core", None);
            branch.state = branch_state.to_owned();
            branch.node_id = Some(node_id.to_owned());
            manifest.branches.insert(branch.name.clone(), branch);
        }
        store::save(
            &state.pool,
            &TreeJob {
                id: JOB.to_owned(),
                root_node_id: "wrap-root".to_owned(),
                project_id: None,
                feature_branch: Some("feat/wrap".to_owned()),
                goal: "Wrap the run".to_owned(),
                state: job_state.to_owned(),
                manifest,
                contract: None,
                created_at: CREATED.to_owned(),
                updated_at: CREATED.to_owned(),
            },
        )
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO work_nodes (id, root_id, depth, kind, title, goal, status, run_id, \
                                     tree_job_id, handoff_path, created_at, updated_at) \
             VALUES (?, ?, ?, ?, 'title', 'goal', 'running', ?, ?, ?, ?, ?)",
        )
        .bind(node_id)
        .bind("wrap-root")
        .bind((branch_state.is_none()) as i64)
        .bind(if branch_state.is_some() { "orchestrator" } else { "root" })
        .bind(run_id.map(|id| id.to_string()))
        .bind(JOB)
        .bind("C:/state/nodes/wrap/handoff.md")
        .bind(CREATED)
        .bind(CREATED)
        .execute(&state.pool)
        .await
        .unwrap();
    }

    async fn seed_run(state: &AppState, id: i64, status: &str) {
        sqlx::query("INSERT INTO runs (id, prompt, status, created_at) VALUES (?, 'original prompt', ?, ?)")
            .bind(id)
            .bind(status)
            .bind(CREATED)
            .execute(&state.pool)
            .await
            .unwrap();
    }

    async fn seed_launch(state: &AppState, node_id: &str) {
        open::save_launch(
            &state.pool,
            &StoredLaunch {
                node_id: node_id.to_owned(),
                key: "initial".to_owned(),
                prompt: "original prompt".to_owned(),
                project_id: None,
                cwd: None,
                mode: "shadow".to_owned(),
            },
        )
        .await
        .unwrap();
    }

    async fn report(state: &AppState, node_id: &str, run_id: i64, result: &str) {
        node_run::apply(&state.pool, node_id, "report", &run_id.to_string(), result)
            .await
            .unwrap();
    }

    async fn launches(state: &AppState, node_id: &str, prefix: &str) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM tree_transitions WHERE node_id = ? AND transition = 'launch' AND key LIKE ?")
            .bind(node_id)
            .bind(format!("{prefix}%"))
            .fetch_one(&state.pool)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn a_dead_run_with_a_handoff_report_is_relaunched_under_a_wrap_key_with_the_handoff_sentence() {
        let state = state().await;
        seed_node(&state, Some("building"), "open", NODE, Some(41)).await;
        seed_run(&state, 41, "interrupted").await;
        seed_launch(&state, NODE).await;
        report(&state, NODE, 41, "handoff").await;

        tick(&state).await;

        assert_eq!(launches(&state, NODE, "wrap-41").await, 1);
        let prompt: String = sqlx::query_scalar("SELECT prompt FROM runs WHERE id <> 41 ORDER BY id DESC LIMIT 1")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert!(prompt.contains("handoff.md"), "{prompt:?}");
    }

    #[tokio::test]
    async fn a_second_tick_over_the_same_dead_run_launches_nothing() {
        let state = state().await;
        seed_node(&state, Some("building"), "open", NODE, Some(42)).await;
        seed_run(&state, 42, "interrupted").await;
        seed_launch(&state, NODE).await;
        report(&state, NODE, 42, "handoff").await;

        tick(&state).await;
        let first = launches(&state, NODE, "wrap-42").await;
        tick(&state).await;

        assert_eq!(first, 1);
        assert_eq!(launches(&state, NODE, "wrap-42").await, first);
    }

    #[tokio::test]
    async fn a_live_run_with_a_handoff_report_is_ended_not_relaunched() {
        let state = state().await;
        seed_node(&state, Some("building"), "open", NODE, None).await;
        let run_id = node_run::launch(&state, NODE, "live", "original prompt".to_owned(), None, None, "shadow")
            .await
            .unwrap();
        report(&state, NODE, run_id, "handoff").await;

        tick(&state).await;

        let status: String = sqlx::query_scalar("SELECT status FROM runs WHERE id = ?")
            .bind(run_id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(status, "cancelled");
        assert_eq!(launches(&state, NODE, "wrap-").await, 0);
    }

    #[tokio::test]
    async fn the_fourth_handoff_link_of_a_chain_stops_with_one_feed_line_and_parks_the_branch() {
        let state = state().await;
        seed_node(&state, Some("building"), "open", NODE, Some(43)).await;
        seed_run(&state, 43, "interrupted").await;
        seed_launch(&state, NODE).await;
        report(&state, NODE, 43, "handoff").await;
        for key in ["wrap-old-1", "wrap-old-2", "wrap-old-3"] {
            node_run::apply(&state.pool, NODE, "launch", key, "99").await.unwrap();
        }

        tick(&state).await;

        let parked = store::load(&state.pool, JOB)
            .await
            .unwrap()
            .unwrap()
            .manifest
            .branches["branch"]
            .state
            .clone();
        assert_eq!(parked, "parked");
        let feed: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM feed WHERE kind = 'node_continuation_stopped' AND subject = ?")
            .bind(format!("node:{NODE}"))
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(feed, 1);
    }

    #[tokio::test]
    async fn a_run_that_died_without_a_report_is_relaunched_twice_and_then_parked() {
        let state = state().await;
        seed_node(&state, Some("building"), "open", NODE, Some(44)).await;
        seed_run(&state, 44, "interrupted").await;
        seed_launch(&state, NODE).await;

        tick(&state).await;
        assert_eq!(launches(&state, NODE, "died-44").await, 1);
        let first: i64 = sqlx::query_scalar("SELECT CAST(run_id AS INTEGER) FROM work_nodes WHERE id = ?")
            .bind(NODE)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        sqlx::query("UPDATE runs SET status = 'interrupted' WHERE id = ?").bind(first).execute(&state.pool).await.unwrap();
        tick(&state).await;
        assert_eq!(launches(&state, NODE, "died-").await, 2);
        let second: i64 = sqlx::query_scalar("SELECT CAST(run_id AS INTEGER) FROM work_nodes WHERE id = ?")
            .bind(NODE)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        sqlx::query("UPDATE runs SET status = 'interrupted' WHERE id = ?").bind(second).execute(&state.pool).await.unwrap();
        tick(&state).await;

        let parked = store::load(&state.pool, JOB)
            .await
            .unwrap()
            .unwrap()
            .manifest
            .branches["branch"]
            .state
            .clone();
        assert_eq!(parked, "parked");
    }

    #[tokio::test]
    async fn a_parked_branch_a_merged_branch_and_a_finished_job_are_never_relaunched() {
        let parked_state = state().await;
        seed_node(&parked_state, Some("parked"), "open", NODE, Some(45)).await;
        seed_run(&parked_state, 45, "interrupted").await;
        seed_launch(&parked_state, NODE).await;
        report(&parked_state, NODE, 45, "handoff").await;

        tick(&parked_state).await;

        assert_eq!(launches(&parked_state, NODE, "wrap-").await, 0);

        let merged_state = state().await;
        seed_node(&merged_state, Some("merged"), "open", NODE, Some(48)).await;
        seed_run(&merged_state, 48, "interrupted").await;
        seed_launch(&merged_state, NODE).await;
        report(&merged_state, NODE, 48, "handoff").await;
        tick(&merged_state).await;
        assert_eq!(launches(&merged_state, NODE, "wrap-").await, 0);

        let finished_state = state().await;
        seed_node(&finished_state, Some("building"), "finished", NODE, Some(49)).await;
        seed_run(&finished_state, 49, "interrupted").await;
        seed_launch(&finished_state, NODE).await;
        report(&finished_state, NODE, 49, "handoff").await;
        tick(&finished_state).await;
        assert_eq!(launches(&finished_state, NODE, "wrap-").await, 0);
    }

    #[tokio::test]
    async fn a_done_report_with_a_dead_run_is_left_to_the_driver() {
        let state = state().await;
        seed_node(&state, Some("building"), "open", NODE, Some(46)).await;
        seed_run(&state, 46, "interrupted").await;
        seed_launch(&state, NODE).await;
        report(&state, NODE, 46, "done").await;

        tick(&state).await;

        assert_eq!(launches(&state, NODE, "wrap-").await, 0);
        let run_id: String = sqlx::query_scalar("SELECT run_id FROM work_nodes WHERE id = ?")
            .bind(NODE)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(run_id, "46");
    }

    #[tokio::test]
    async fn the_root_node_is_relaunched_after_a_handoff_report() {
        let state = state().await;
        seed_node(&state, None, "open", "wrap-root", Some(47)).await;
        seed_run(&state, 47, "interrupted").await;
        seed_launch(&state, "wrap-root").await;
        report(&state, "wrap-root", 47, "handoff").await;

        tick(&state).await;

        assert_eq!(launches(&state, "wrap-root", "wrap-47").await, 1);
    }

    /// A relaunch starts the node from the launch its first start stored, so it carries that launch's
    /// mode and worktree. A node that was first started in plan mode would be started again in plan
    /// mode, and would plan, end, and be wrapped again until the chain's cap parks the branch.
    ///
    /// Goes through the real root start (the route) and the real wrap, so that the launch the wrap
    /// reads is the one `root::start` stored, and no signature is named that GREEN may still change.
    /// The driver's fix and answer relaunches take the same road (`open::relaunch_node` to
    /// `store::launch_recovering`), so this one pin covers them.
    #[tokio::test]
    async fn a_wrap_relaunch_is_not_launched_in_plan_mode() {
        use axum::body::Body;
        use axum::http::Request;
        use tower::ServiceExt;

        use crate::execution::git_exec::testkit::{initialize_repo, space_free_tempdir};
        use crate::execution::runner::Permission;

        let container = space_free_tempdir("tree-wrap-plan-");
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
            Some(Duration::from_secs(60)),
            Duration::from_secs(600),
        )
        .await;
        runner.canned.lock().unwrap().as_mut().unwrap().session_id = None;
        let request = Request::builder()
            .method("POST")
            .uri("/trees")
            .header("Authorization", "Bearer test-token")
            .header("Content-Type", "application/json")
            .body(Body::from(
                serde_json::json!({
                    "id": JOB,
                    "goal": "Wrap the root",
                    "feature_branch": "feat/wrap",
                    "project_root": repo.to_string_lossy().replace('\\', "/"),
                    "project_id": "project-0",
                })
                .to_string(),
            ))
            .unwrap();
        let response = crate::tree_job::http::routes()
            .with_state(state.clone())
            .oneshot(request)
            .await
            .unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::OK);
        let root = store::load(&state.pool, JOB).await.unwrap().unwrap().root_node_id;
        let first: i64 = sqlx::query_scalar("SELECT CAST(run_id AS INTEGER) FROM work_nodes WHERE id = ?")
            .bind(&root)
            .fetch_one(&state.pool)
            .await
            .expect("the root run is linked to the root node");
        for _ in 0..300 {
            if runner.last_permission.lock().unwrap().is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        // The first run ended after a handoff report; the wrap has to start the node again.
        sqlx::query("UPDATE runs SET status = 'interrupted' WHERE id = ?")
            .bind(first)
            .execute(&state.pool)
            .await
            .unwrap();
        sqlx::query("UPDATE work_nodes SET status = 'running' WHERE id = ?")
            .bind(&root)
            .execute(&state.pool)
            .await
            .unwrap();
        report(&state, &root, first, "handoff").await;
        *runner.last_permission.lock().unwrap() = None;

        tick(&state).await;

        assert_eq!(launches(&state, &root, "wrap-").await, 1, "the wrap relaunched the root");
        let mut seen = None;
        for _ in 0..300 {
            seen = *runner.last_permission.lock().unwrap();
            if seen.is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let seen = seen.expect("the relaunched run reached the runner");
        assert_ne!(
            seen,
            Permission::Plan,
            "a relaunch that plans only ends again and burns the chain's cap"
        );
    }
}
