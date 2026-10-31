//! Tree-job root node handling: the top-level node of a job. `start` gives it a worktree on its own
//! git branch, `tree/<id>/root` cut from the feature branch, and launches the run that plans the job.

use super::http::job_lock;
use super::model::Branch;
use super::open;
use super::store::{self, TreeJob};
use crate::state::AppState;

/// The manifest name of the root's branch. The worktree is `<root>/.claude/worktrees/<id>-root`.
const ROOT_BRANCH: &str = "root";

/// What the root run is told besides the goal. It must stay true to the tools and to what
/// `close_node` accepts: the daemon reads the report from the file at `handoff_path` (the MCP tool
/// sends `status` and `summary` too, but a tree node's are ignored), and a root that reports
/// `split` on an unfinished job would be closed, so `done` is the only outcome it uses.
const ROOT_PROTOCOL: &str = "\
You are the root of a tree job. You plan and delegate; you write no code yourself.

Work in this worktree (your current directory). It is on its own git branch cut from the feature \
branch. Read the project as needed, then split the goal into independent parts that touch disjoint \
paths.

Open one branch per part with the `tree_branch_start` tool. Its body is {name, goal, owned_paths, \
prompt, layer?, base?, node_id}: `name` is a short branch name (letters, digits, '.', '_' or '-'), \
`goal` and `prompt` describe the part and the task of the agent that will work on it, `owned_paths` \
lists the paths that part may write, `layer` and `base` are optional (`base` names a branch that \
must merge first), and `node_id` is your own Node ID. Each call creates the branch, its worktree \
and its agent, and starts it. Opening the same branch twice is safe.

You cannot wait for your branches, and you never merge, land or push anything: the daemon gates \
and merges each branch when its agent reports, then lands the job. When every part is opened, \
report and stop.

To report, write your report to a file of your own: a new file inside this worktree, for example \
`.nucleos-root-report.md`, which the daemon keeps out of git (do not commit it). Do not use `<state_dir>/nodes/<root>/<id>.md`: \
the daemon writes that file itself and overwrites yours. Then call `node_close` with `handoff_path` \
set to the absolute path of your file, and with `status` `done` and a one-line `summary`. The \
daemon reads the report from the file and ignores those two fields, but the tool requires them. \
The report has at most 60 non-blank lines, and looks like this:

# Node Handoff

Node ID: <your Node ID>
Root ID: <your Root ID>
Status: done
Role: orchestrator
Outcome: done
Contract used: <ids such as A3, E2 separated by commas, or none>

## Summary
<what you opened and why, in at most 1500 characters>

## What was done
- <one bullet per branch you opened>

## Decisions
- <or none>

## Files touched
none

## Commands for the root
- <or none>

## Open items / next step
<or none>

Your Outcome is always `done` once your branches are open, with Status `done` and Role \
`orchestrator`. Never use the outcome `split`. An orchestrator edits no file, so Files touched is \
`none`. When `node_close` answers with \"recorded\": true, your report is stored: stop at once and \
do nothing more. If it answers with a list of problems, fix the file and call it again.

If your context is nearly full before you finish, write the same file with Status `handoff` and \
Outcome `handoff`, put what is left in `## Open items / next step`, and call the `handoff` tool: a \
new agent continues from your report. If you cannot proceed, report Status `blocked` and Outcome \
`blocked` with the cause under `## Blocked reason`.";

/// The prompt of the root run: the goal, then the protocol, then the ids the report header needs.
fn root_prompt(job: &TreeJob) -> String {
    format!(
        "{}\n\n{ROOT_PROTOCOL}\n\nYour Node ID and your Root ID are both {}.",
        job.goal, job.root_node_id
    )
}

/// Puts the project's pinned workflow in the new worktree, as the open route does. A failure is
/// logged and never fails the start; a test build starts no process.
async fn workflow_sync(worktree: &str) {
    let result = open::workflow_sync(worktree).await;
    if result.starts_with("failed") {
        tracing::warn!(%result, "root worktree workflow sync failed");
    }
}

/// Adds the `root` branch (layer `code`, no paths, `queued`) unless the manifest has it already.
async fn add_root_branch(state: &AppState, job_id: &str) -> Result<(), String> {
    let lock = job_lock(job_id);
    let _guard = lock.lock().await;
    let mut job = store::load(&state.pool, job_id)
        .await
        .map_err(|error| error.to_string())?
        .ok_or_else(|| format!("tree job {job_id} not found"))?;
    if job.manifest.branches.contains_key(ROOT_BRANCH) {
        return Ok(());
    }
    job.manifest
        .branches
        .insert(ROOT_BRANCH.to_owned(), Branch::new(ROOT_BRANCH, &[], "code", None));
    job.updated_at = chrono::Utc::now().to_rfc3339();
    store::update(&state.pool, &job)
        .await
        .map_err(|error| error.to_string())
}

/// Takes the `root` branch out again when it is still `queued` and unbound, so that a failed open
/// does not leave a branch the driver would wait on. Best effort.
async fn drop_unopened_root_branch(state: &AppState, job_id: &str) {
    let lock = job_lock(job_id);
    let _guard = lock.lock().await;
    let Ok(Some(mut job)) = store::load(&state.pool, job_id).await else {
        return;
    };
    let unopened = job
        .manifest
        .branches
        .get(ROOT_BRANCH)
        .is_some_and(|branch| branch.state == "queued" && branch.node_id.is_none());
    if unopened {
        job.manifest.branches.remove(ROOT_BRANCH);
        job.updated_at = chrono::Utc::now().to_rfc3339();
        let _ = store::update(&state.pool, &job).await;
    }
}

/// Starts the root run of `job`: the `root` branch, its worktree on `tree/<id>/root`, the root node
/// bound to it, and one `shadow` run in that worktree under the launch key `root-<id>`. Every step
/// replays, so calling it again after a failure finishes the missing ones and never starts a second
/// run. It takes the job lock itself (in `open_branch` and `bind_branch`), so the caller must not
/// hold it. Answers the run id.
pub async fn start(state: &AppState, job: &TreeJob) -> Result<i64, String> {
    let id = job.id.as_str();
    add_root_branch(state, id).await?;
    let config = open::open_config(&job.manifest.project_root).await;
    let opened = match open::open_branch(&state.pool, id, ROOT_BRANCH, &config).await {
        Ok(opened) => opened,
        Err(error) => {
            drop_unopened_root_branch(state, id).await;
            return Err(error.to_string());
        }
    };
    let worktree = opened
        .worktree
        .ok_or_else(|| "the root branch opened with no worktree".to_owned())?;
    if !opened.replayed {
        workflow_sync(&worktree).await;
    }
    open::bind_branch(&state.pool, id, ROOT_BRANCH, &job.root_node_id)
        .await
        .map_err(|error| error.to_string())?;
    store::launch_recovering(
        state,
        &job.root_node_id,
        &format!("root-{id}"),
        root_prompt(job),
        job.project_id.clone(),
        Some(worktree),
        "shadow",
    )
    .await
    .map_err(|error| error.to_string())
}

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
