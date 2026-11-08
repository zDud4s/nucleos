//! Tree-job root node handling: the top-level node of a job. `start` gives it a worktree on its own
//! git branch, `tree/<id>/root` cut from the feature branch, and launches the run that plans the job.

use serde_json::{Value, json};

use super::decisions;
use super::http::job_lock;
use super::model::Branch;
use super::open;
use super::store::{self, TreeJob};
use crate::state::AppState;

/// The manifest name of the root's branch. The worktree is `<root>/.claude/worktrees/<id>-root`.
pub(super) const ROOT_BRANCH: &str = "root";

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
and its agent, and starts it. Opening the same branch twice is safe. Each started part must pass \
the gate on its own, so a part that needs another part's code must name that branch as `base`. \
Open every part now, later ones too: a part whose base has not merged answers `queued` with \
`waiting_for`, and the daemon starts it by itself once the base merges. Leave no part for later.

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
        "{}\n\n{ROOT_PROTOCOL}\n\n{}\n\nYour Node ID and your Root ID are both {}. The tree job id is {}: pass it as `id` to `tree_branch_start` (not your Node ID), with your Node ID as `node_id`.",
        job.goal,
        super::node_run::TICKET_RULES,
        job.root_node_id,
        job.id
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
    job.manifest.branches.insert(
        ROOT_BRANCH.to_owned(),
        Branch::new(ROOT_BRANCH, &[], "code", None),
    );
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

/// `manifest.extra` key of a root start that failed: `{ "attempts", "error", "at" }`, plus
/// `decision_id` once the owner was asked. `start` writes it on a failure and removes it on a
/// success; `retry_failed_starts` reads it.
pub(super) const ROOT_START_KEY: &str = "root_start";
/// Failed starts of a root (the route's own first try included) before the owner is asked.
const ROOT_START_CAP: u64 = 5;

/// Starts the root run of `job`: the `root` branch, its worktree on `tree/<id>/root`, the root node
/// bound to it, and one `worktree` run in that worktree under the launch key `root-<id>`. Every step
/// replays, so calling it again after a failure finishes the missing ones and never starts a second
/// run. It takes the job lock itself (in `open_branch` and `bind_branch`), so the caller must not
/// hold it. Answers the run id.
///
/// A failure is recorded on the job (`ROOT_START_KEY`), so the sweep starts the root again
/// (`retry_failed_starts`) instead of leaving an open job that nothing moves.
pub async fn start(state: &AppState, job: &TreeJob) -> Result<i64, String> {
    let outcome = start_steps(state, job).await;
    note_start(state, &job.id, &outcome).await;
    outcome
}

/// Records a failed start on the job, one attempt more, or removes the record after a success.
/// Best effort: a failed write is logged, and the start's own outcome stays the answer.
async fn note_start(state: &AppState, job_id: &str, outcome: &Result<i64, String>) {
    edit_start_record(state, job_id, |extra| match outcome {
        Ok(_) => extra.remove(ROOT_START_KEY).is_some(),
        Err(error) => {
            let record = extra
                .entry(ROOT_START_KEY.to_owned())
                .or_insert_with(|| json!({}));
            if !record.is_object() {
                *record = json!({});
            }
            let attempts = record["attempts"].as_u64().unwrap_or(0) + 1;
            record["attempts"] = json!(attempts);
            record["error"] = json!(error);
            record["at"] = json!(chrono::Utc::now().to_rfc3339());
            true
        }
    })
    .await;
}

/// Applies `edit` to the job's `manifest.extra` under the job's lock, and writes the job when
/// `edit` answers that it changed something. Best effort, logged.
async fn edit_start_record(
    state: &AppState,
    job_id: &str,
    edit: impl FnOnce(&mut std::collections::BTreeMap<String, Value>) -> bool,
) {
    let lock = job_lock(job_id);
    let _guard = lock.lock().await;
    let mut job = match store::load(&state.pool, job_id).await {
        Ok(Some(job)) => job,
        Ok(None) => return,
        Err(error) => {
            tracing::warn!(job = job_id, %error, "could not read the job to record its root start");
            return;
        }
    };
    if !edit(&mut job.manifest.extra) {
        return;
    }
    job.updated_at = chrono::Utc::now().to_rfc3339();
    if let Err(error) = store::update(&state.pool, &job).await {
        tracing::warn!(job = job_id, %error, "could not record the root start of the job");
    }
}

/// Starts again the root of every unfinished job whose start failed (its `ROOT_START_KEY` record).
/// The sweep calls it every tick, through `wrap::tick`. Past `ROOT_START_CAP` failed attempts the
/// owner is asked once on the root (`decisions::park_or_ask`), and nothing is tried while that
/// question is open; its answer is a fresh allowance. A failure on one job is logged and stops no
/// other.
pub async fn retry_failed_starts(state: &AppState) {
    let jobs = match store::list_non_terminal(&state.pool).await {
        Ok(jobs) => jobs,
        Err(error) => {
            tracing::warn!(%error, "could not list the jobs whose root start failed");
            return;
        }
    };
    for job in jobs {
        let Some(record) = job.manifest.extra.get(ROOT_START_KEY).cloned() else {
            continue;
        };
        if let Err(error) = retry_failed_start(state, &job, &record).await {
            tracing::warn!(job = %job.id, %error, "could not retry the root start");
        }
    }
}

async fn retry_failed_start(state: &AppState, job: &TreeJob, record: &Value) -> Result<(), String> {
    let pool = &state.pool;
    let run: Option<Option<String>> =
        sqlx::query_scalar("SELECT run_id FROM work_nodes WHERE id = ?")
            .bind(&job.root_node_id)
            .fetch_optional(pool)
            .await
            .map_err(|error| error.to_string())?;
    match run {
        None => return Ok(()),
        // Started meanwhile (an answer's relaunch, say): the record is stale.
        Some(Some(_)) => {
            edit_start_record(state, &job.id, |extra| {
                extra.remove(ROOT_START_KEY).is_some()
            })
            .await;
            return Ok(());
        }
        Some(None) => {}
    }
    if let Some(decision) = record.get("decision_id").and_then(Value::as_str) {
        let status: Option<String> =
            sqlx::query_scalar("SELECT status FROM decisions WHERE id = ?")
                .bind(decision)
                .fetch_optional(pool)
                .await
                .map_err(|error| error.to_string())?;
        if status.as_deref() == Some(decisions::STATUS_OPEN) {
            return Ok(());
        }
        // Answered: a fresh allowance of attempts.
        edit_start_record(state, &job.id, |extra| {
            match extra.get_mut(ROOT_START_KEY).and_then(Value::as_object_mut) {
                Some(record) => {
                    record.remove("decision_id");
                    record.insert("attempts".to_owned(), json!(0));
                    true
                }
                None => false,
            }
        })
        .await;
    } else {
        let attempts = record["attempts"].as_u64().unwrap_or(0);
        let error = record["error"].as_str().unwrap_or("unknown");
        if attempts >= super::root_disk::start_cap(error, ROOT_START_CAP) {
            let question = super::root_disk::question(&job.id, attempts, error);
            let asked = decisions::park_or_ask(pool, &job.id, &job.root_node_id, &question)
                .await
                .map_err(|error| error.to_string())?;
            if let Some(asked) = asked {
                edit_start_record(state, &job.id, |extra| {
                    match extra.get_mut(ROOT_START_KEY).and_then(Value::as_object_mut) {
                        Some(record) => {
                            record.insert("decision_id".to_owned(), json!(asked.id));
                            true
                        }
                        None => false,
                    }
                })
                .await;
            }
            return Ok(());
        }
    }
    // A refused admission waits out a backoff before the next try (see `watchdog`).
    if super::watchdog::root_start_waits(record) {
        return Ok(());
    }
    start(state, job).await.map(|_| ())
}

async fn start_steps(state: &AppState, job: &TreeJob) -> Result<i64, String> {
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
        "worktree",
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

    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    use crate::execution::git_exec::testkit::{initialize_repo, space_free_tempdir};
    use crate::execution::runner::{FakeCommandRunner, Permission};
    use crate::state::AppState;
    use crate::tree_job::http::routes;

    const GOAL: &str = "Ship the unmistakable goal";
    const TREE: &str = "job-1";

    /// `git <args>` in `dir`, stdout trimmed; asserts a zero exit.
    fn git_text(dir: &Path, args: &[&str]) -> String {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .expect("git should start");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }

    /// A repository on `master` with a zero disk floor. The root branch is still charged its 8 GB
    /// seed cost; what admits it whatever the disk holds is the test build's fixed free disk
    /// (`open::free_disk_gb`). The same set-up the prompt test above does inline.
    fn tree_repo(container: &tempfile::TempDir) -> PathBuf {
        let repo = container.path().join("repo");
        initialize_repo(&repo);
        git_text(&repo, &["branch", "-M", "master"]);
        let workflow = repo.join(".nucleos/workflow");
        std::fs::create_dir_all(&workflow).unwrap();
        std::fs::write(workflow.join("speed.yaml"), "tree:\n  floor_gb: 0\n").unwrap();
        repo
    }

    /// Creates the tree job through the route, which starts its root run in the root worktree.
    async fn start_tree(
        repo: &Path,
        delay: Option<Duration>,
    ) -> (AppState, Arc<FakeCommandRunner>) {
        let (state, runner) = crate::execution::runs::testkit::test_state_with_runner(
            delay,
            Duration::from_secs(600),
        )
        .await;
        runner.canned.lock().unwrap().as_mut().unwrap().session_id = None;
        let body = json!({
            "id": TREE,
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
        let response = routes()
            .with_state(state.clone())
            .oneshot(request)
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        (state, runner)
    }

    /// The permission and the directory the runner was launched with, once there is a launch.
    async fn first_launch(runner: &FakeCommandRunner) -> Option<(Permission, PathBuf)> {
        // Hang guard only: wait for the launch, not for a short budget.
        let deadline = std::time::Instant::now() + Duration::from_secs(60);
        while std::time::Instant::now() < deadline {
            let permission = *runner.last_permission.lock().unwrap();
            let cwd = runner.last_cwd.lock().unwrap().clone();
            if let (Some(permission), Some(cwd)) = (permission, cwd) {
                return Some((permission, cwd));
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        None
    }

    /// The worktree the manifest records for the `root` branch.
    async fn root_worktree(state: &AppState) -> PathBuf {
        let job = crate::tree_job::store::load(&state.pool, TREE)
            .await
            .unwrap()
            .expect("the tree job is stored");
        PathBuf::from(
            job.manifest.branches["root"]
                .worktree
                .clone()
                .expect("the root branch has a worktree"),
        )
    }

    /// A tree job's root run is the agent that must call `tree_branch_start` and `node_close` and
    /// write its report, so a launch in plan mode (`--permission-mode plan`) can do none of it: the
    /// agent plans, ends, and the wrap parks. With no classifier wired at the project root the
    /// permission is the plain `Default`: neither `Plan`, nor an ungoverned `Bypass`.
    #[tokio::test]
    async fn a_tree_root_run_is_not_launched_in_plan_mode() {
        let container = space_free_tempdir("tree-root-plan-");
        let repo = tree_repo(&container);
        let (_state, runner) = start_tree(&repo, Some(Duration::from_secs(5))).await;

        let (permission, _cwd) = first_launch(&runner)
            .await
            .expect("the root run reached the runner");

        assert_ne!(
            permission,
            Permission::Plan,
            "a tree agent launched in plan mode cannot call its tools or write"
        );
        assert_eq!(
            permission,
            Permission::Default,
            "no hook is wired at this root, so nothing may stand the CLI's barrier down"
        );
    }

    /// The root run works in the worktree `open_branch` made for it, as a run of the mode every
    /// autonomous code run has, and no second checkout is provisioned for it.
    #[tokio::test]
    async fn a_tree_root_run_works_in_the_tree_worktree_without_provisioning_another() {
        let container = space_free_tempdir("tree-root-cwd-");
        let repo = tree_repo(&container);
        let (state, runner) = start_tree(&repo, Some(Duration::from_secs(5))).await;
        let (_, cwd) = first_launch(&runner)
            .await
            .expect("the root run reached the runner");

        let worktree = root_worktree(&state).await;

        assert_eq!(cwd, worktree, "the run starts in the tree's own worktree");
        let (mode, run_cwd): (String, Option<String>) =
            sqlx::query_as("SELECT mode, cwd FROM runs ORDER BY id LIMIT 1")
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(mode, "worktree", "a tree run is an autonomous code run");
        assert_eq!(run_cwd.map(PathBuf::from), Some(worktree));
        let listed = git_text(&repo, &["worktree", "list", "--porcelain"]);
        let trees = listed
            .lines()
            .filter(|line| line.starts_with("worktree "))
            .count();
        assert_eq!(
            trees, 2,
            "the project checkout and the root worktree, and no third one: {listed}"
        );
    }

    /// A finished tree run is announced as the worktree run it is; `shadow_run_completed` is the
    /// feed line of a planning run.
    #[tokio::test]
    async fn a_tree_run_completion_is_not_a_shadow_feed_entry() {
        let container = space_free_tempdir("tree-root-feed-");
        let repo = tree_repo(&container);
        let (state, _runner) = start_tree(&repo, None).await;

        let mut kinds: Vec<String> = Vec::new();
        for _ in 0..300 {
            // `list_feed(None)` returns only global rows, and this run's line carries its project.
            kinds = sqlx::query_scalar("SELECT kind FROM feed")
                .fetch_all(&state.pool)
                .await
                .unwrap();
            if kinds.iter().any(|kind| kind.ends_with("_run_completed")) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        assert!(
            kinds.iter().any(|kind| kind == "worktree_run_completed"),
            "the root run's completion is a worktree run's: {kinds:?}"
        );
        assert!(
            !kinds.iter().any(|kind| kind == "shadow_run_completed"),
            "{kinds:?}"
        );
    }

    /// The project root wires the classifier hook, and `.claude/` is not committed, so a worktree
    /// carries no `settings.json` until the run inherits one. Without it the CLI's barrier would
    /// either stay up (nobody to answer it) or come down with nothing watching.
    #[tokio::test]
    async fn a_tree_run_is_governed_by_the_classifier_hook() {
        let container = space_free_tempdir("tree-root-hook-");
        let repo = tree_repo(&container);
        crate::governance::autopilot::wire_classifier_hook(&repo).unwrap();
        let (_state, runner) = start_tree(&repo, Some(Duration::from_secs(5))).await;

        let (permission, cwd) = first_launch(&runner)
            .await
            .expect("the root run reached the runner");

        assert!(
            crate::governance::autopilot::classifier_hook_is_wired(&cwd),
            "the tree worktree must carry the hook its project root wires"
        );
        assert_eq!(permission, Permission::Bypass);
        assert_eq!(
            *runner.last_classifier_governs_tools.lock().unwrap(),
            Some(true),
            "Bypass is only safe because the classifier governs the tools"
        );
    }

    /// `workflow --sync` leaves `.agents/` and `AGENTS.md` untracked in a tree worktree, and
    /// `checkpoint` is `git add -A`: without an exclusion the agents' workflow files would be
    /// committed into the feature branch and merged, as the report files would have been.
    #[tokio::test]
    async fn workflow_files_stay_out_of_a_tree_checkpoint() {
        let container = space_free_tempdir("tree-root-sync-");
        let repo = tree_repo(&container);
        let (state, _runner) = start_tree(&repo, Some(Duration::from_secs(5))).await;
        let worktree = root_worktree(&state).await;
        std::fs::create_dir_all(worktree.join(".agents")).unwrap();
        std::fs::write(worktree.join(".agents/x"), "workflow\n").unwrap();
        std::fs::write(worktree.join("AGENTS.md"), "# managed\n").unwrap();
        let before = git_text(&worktree, &["rev-parse", "HEAD"]);

        let status = git_text(
            &worktree,
            &["status", "--porcelain", "--untracked-files=all"],
        );
        let sha = crate::execution::worktree::checkpoint(&worktree)
            .await
            .unwrap();

        assert!(
            !status.contains("AGENTS.md") && !status.contains(".agents"),
            "workflow files show as changes of the tree: {status:?}"
        );
        assert_eq!(
            sha, before,
            "a checkpoint of workflow files alone commits nothing"
        );
        let tracked = git_text(&worktree, &["ls-files"]);
        assert!(
            !tracked.contains("AGENTS.md") && !tracked.contains(".agents"),
            "{tracked:?}"
        );
    }

    #[test]
    fn the_root_prompt_carries_the_ticket_rules() {
        let job = super::TreeJob {
            id: "proof-slugify-3".to_owned(),
            root_node_id: "9f1c2d3e-root-node-uuid".to_owned(),
            project_id: None,
            feature_branch: Some("feat/tree".to_owned()),
            goal: GOAL.to_owned(),
            state: "open".to_owned(),
            manifest: crate::tree_job::model::Manifest::new(
                "proof-slugify-3",
                "feat/tree",
                GOAL,
                "C:/Projects/nucleos",
                &[],
            ),
            contract: None,
            created_at: "2026-01-01T00:00:00Z".to_owned(),
            updated_at: "2026-01-01T00:00:00Z".to_owned(),
        };

        let prompt = super::root_prompt(&job);

        assert!(
            prompt.contains(crate::tree_job::node_run::TICKET_RULES),
            "{prompt}"
        );
    }

    /// The root agent must be told the tree job's id (not only its root node uuid), or it passes the
    /// node uuid as `id` to `tree_branch_start` and the door answers 403.
    #[test]
    fn the_root_prompt_names_its_tree_job_id() {
        let job = super::TreeJob {
            id: "proof-slugify-3".to_owned(),
            root_node_id: "9f1c2d3e-root-node-uuid".to_owned(),
            project_id: None,
            feature_branch: Some("feat/tree".to_owned()),
            goal: GOAL.to_owned(),
            state: "open".to_owned(),
            manifest: crate::tree_job::model::Manifest::new(
                "proof-slugify-3",
                "feat/tree",
                GOAL,
                "C:/Projects/nucleos",
                &[],
            ),
            contract: None,
            created_at: "2026-01-01T00:00:00Z".to_owned(),
            updated_at: "2026-01-01T00:00:00Z".to_owned(),
        };

        let prompt = super::root_prompt(&job);

        assert!(prompt.contains("proof-slugify-3"), "{prompt}");
        assert!(prompt.contains("tree_branch_start"), "{prompt}");
    }

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

        let response = routes()
            .with_state(state.clone())
            .oneshot(request)
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let prompts: Vec<String> = sqlx::query_scalar("SELECT prompt FROM runs")
            .fetch_all(&state.pool)
            .await
            .unwrap();
        assert_eq!(
            prompts.len(),
            1,
            "expected exactly one root run: {prompts:?}"
        );
        let prompt = &prompts[0];
        assert!(prompt.contains(GOAL), "the goal is missing: {prompt}");
        // The protocol names the tools the root uses and the way it ends.
        for needle in ["tree_branch_start", "node_close", "handoff"] {
            assert!(
                prompt.contains(needle),
                "the protocol lacks {needle:?}: {prompt}"
            );
        }
    }

    // ---- a root start that failed ------------------------------------------------------------------

    /// The root node of the job and its run id, if it has one.
    async fn root_run(state: &AppState) -> (String, Option<String>) {
        sqlx::query_as(
            "SELECT n.id, n.run_id FROM work_nodes n \
             JOIN tree_jobs j ON j.root_node_id = n.id WHERE j.id = ?",
        )
        .bind(TREE)
        .fetch_one(&state.pool)
        .await
        .unwrap()
    }

    /// `(id, question)` of every open decision on `node_id`.
    async fn open_decisions(state: &AppState, node_id: &str) -> Vec<(String, String)> {
        sqlx::query_as("SELECT id, question FROM decisions WHERE node_id = ? AND status = 'open'")
            .bind(node_id)
            .fetch_all(&state.pool)
            .await
            .unwrap()
    }

    /// `POST /trees` answers 200 when the root cannot start (here: the project root is not there
    /// yet), and the job stays open. The 60-second sweep starts it once it can, instead of leaving an
    /// open job with no run that nothing ever moves again.
    /// Moves a recorded root-start failure back in time, past any admission backoff, so the next
    /// sweep may retry it at once.
    async fn age_start_record(state: &AppState) {
        super::edit_start_record(state, TREE, |extra| {
            match extra
                .get_mut(super::ROOT_START_KEY)
                .and_then(serde_json::Value::as_object_mut)
            {
                Some(record) => {
                    let old = chrono::Utc::now() - chrono::Duration::hours(2);
                    record.insert("at".to_owned(), json!(old.to_rfc3339()));
                    true
                }
                None => false,
            }
        })
        .await;
    }

    #[tokio::test]
    async fn a_root_start_that_failed_is_retried_by_the_sweep() {
        let container = space_free_tempdir("tree-root-retry-");
        let missing = container.path().join("repo");
        let (state, _runner) = start_tree(&missing, Some(Duration::from_secs(5))).await;
        let (_, run) = root_run(&state).await;
        assert_eq!(run, None, "no repository yet: the start failed");

        tree_repo(&container);
        age_start_record(&state).await;
        crate::tree_job::wrap::tick(&state).await;

        let (_, run) = root_run(&state).await;
        assert!(run.is_some(), "the sweep started the root");
    }

    /// A root that keeps failing to start is not retried forever in silence: past a cap the owner
    /// is asked once, and an answer is a fresh allowance of retries.
    #[tokio::test]
    async fn a_root_start_that_keeps_failing_asks_the_owner_once_and_an_answer_retries_it() {
        let container = space_free_tempdir("tree-root-ask-");
        let missing = container.path().join("repo");
        let (state, _runner) = start_tree(&missing, Some(Duration::from_secs(5))).await;

        for _ in 0..10 {
            age_start_record(&state).await;
            crate::tree_job::wrap::tick(&state).await;
        }

        let (root, run) = root_run(&state).await;
        assert_eq!(run, None);
        let asked = open_decisions(&state, &root).await;
        assert_eq!(asked.len(), 1, "asked once: {asked:?}");
        let state_of_job: String = sqlx::query_scalar("SELECT state FROM tree_jobs WHERE id = ?")
            .bind(TREE)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(state_of_job, "open");

        tree_repo(&container);
        crate::tree_job::open::answer_and_relaunch(&state, &asked[0].0, "it is there now", "shell")
            .await
            .unwrap();
        age_start_record(&state).await;
        crate::tree_job::wrap::tick(&state).await;

        let (_, run) = root_run(&state).await;
        assert!(run.is_some(), "the answer gave the start a fresh try");
    }
}
