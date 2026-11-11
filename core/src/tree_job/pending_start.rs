//! Pending starts: replays the branch starts that `branch_start` stored because the disk alone
//! refused them, each sweep of the driver, once the disk (or the base branch it waits for) admits
//! them. A record is consumed, never left behind: a crash between any two writes repeats safely.

use axum::http::StatusCode;
use serde_json::{Value, json};
use sqlx::SqlitePool;

use super::decisions;
use super::driver::{now, persist};
use super::http::job_lock;
use super::model::{JOB_TERMINAL_STATE, Manifest};
use super::store::{self, TreeJob};
use crate::state::AppState;
use crate::work_nodes;

/// Replays each start that `branch_start` stored because the disk alone refused it, once the disk
/// admits the branch. Still short (disk or admission slot): nothing is written or announced and the
/// record stays. The caller is gone: the record is dropped and the decision stays open for the
/// owner. The parent already closed: the branch fails with that reason. Any other failed replay
/// backs off exponentially per branch.
pub(super) async fn retry_pending_starts(state: &AppState, job_id: &str) -> Result<(), String> {
    let Some(job) = store::load(&state.pool, job_id)
        .await
        .map_err(|e| e.to_string())?
    else {
        return Ok(());
    };
    if job.state == JOB_TERMINAL_STATE {
        return Ok(());
    }
    let Some(pending) = job
        .manifest
        .extra
        .get("pending_starts")
        .and_then(Value::as_object)
    else {
        return Ok(());
    };
    for (name, record) in pending {
        // A branch that failed lately waits out its backoff: no attempt, no log line.
        if !is_due(record) {
            continue;
        }
        if let Err(error) = retry_pending_start(state, &job, name, record).await {
            tracing::warn!(job = job_id, branch = %name, %error, "a pending start could not be retried");
            if let Err(stored) = record_failure(&state.pool, job_id, name, &error).await {
                tracing::warn!(job = job_id, branch = %name, error = %stored, "could not record the failed retry");
            }
        }
    }
    Ok(())
}

async fn retry_pending_start(
    state: &AppState,
    job: &TreeJob,
    name: &str,
    record: &Value,
) -> Result<(), String> {
    let pool = &state.pool;
    let caller = record["caller"].as_str().unwrap_or_default();
    let Ok(caller_node) = work_nodes::get(pool, caller).await else {
        return drop_pending_start(pool, &job.id, name).await;
    };
    // A start waiting for its base replays only once that base has merged.
    let waits_for_base = record["waits_for_base"].as_str();
    if let Some(base) = waits_for_base
        && base_is_gone(&job.manifest, base)
    {
        // The base was dropped (or left the tree): it will never merge, so the waiting branch is
        // dropped with the base named, instead of staying queued behind a start nobody retries.
        let reason = format!("base branch {base} was dropped");
        return drop_waiting_branch(pool, &job.id, name, &reason, None).await;
    }
    if let Some(base) = waits_for_base
        && base_still_unmerged(&job.manifest, base)
    {
        return Ok(());
    }
    // A branch already added by an earlier, half-finished replay needs no admission: the replay
    // only finishes its remaining steps.
    if !job.manifest.branches.contains_key(name) {
        // A parent that is already closed can never take the child, however long the start waits:
        // the branch fails with that reason instead of being replayed (and refused) forever. The
        // parent is not reopened: it closed with its own report, and reopening it would rerun
        // work the tree already counted as finished.
        if parent_is_closed(&caller_node.status, caller == job.root_node_id) {
            let reason = format!(
                "parent node {caller} is already {}; it cannot take new children",
                caller_node.status
            );
            return fail_pending_start(state, job, name, record, &reason).await;
        }
        let layer = record["layer"].as_str().unwrap_or("code");
        let admission = super::http::current_admission(&job.manifest, layer).await?;
        if !admission.admit {
            if admission.disk_only {
                // A record a crash left without its decision asks now, once.
                if record["decision_id"].as_str().is_none() {
                    super::branch_start::heal_pending_decision(state, &job.id, name).await?;
                }
            }
            // Disk or slot: either clears by itself later, so the record stays for the next sweep.
            // Dropping it for a missing slot used to strand the whole subtree behind it.
            return Ok(());
        }
    }
    if let Err((status, reason)) =
        super::branch_start::start_pending_status(state, &job.id, name, record).await
    {
        // A request the route will refuse however often it is replayed (an absolute owned path, a
        // bad field) is failed now and its caller told, instead of waiting in the record for ever.
        if refused_for_good(status) {
            let reason = format!("the start was refused ({status}): {reason}");
            fail_pending_start(state, job, name, record, &reason).await?;
            tell_caller(state, job, &caller_node, name, record, &reason).await;
            return Ok(());
        }
        return Err(format!("{status}: {reason}"));
    }

    if waits_for_base.is_some() {
        // No owner question was ever asked for it: the record is consumed and the start announced.
        drop_pending_start(pool, &job.id, name).await?;
        let line = format!(
            "{}: branch {name} started automatically, its base is merged",
            job.goal
        );
        if let Err(error) = crate::feed::append(
            pool,
            job.project_id.as_deref(),
            "work_tree_pending",
            &line,
            None,
            None,
        )
        .await
        {
            tracing::warn!(job = %job.id, %error, "could not announce a started branch");
        }
        return Ok(());
    }

    // Start, then answer, then drop the record: a crash between any two repeats safely. A record
    // with no id may still have a decision (a crash between the ask and the id): the id is fixed.
    let generation = record["generation"].as_u64().unwrap_or(1);
    let fallback = super::branch_start::pending_decision_id(&job.id, name, generation);
    let decision = record["decision_id"].as_str().unwrap_or(&fallback);
    let text = format!("disk freed: branch {name} admitted automatically");
    match decisions::answer(pool, decision, &text, "driver").await {
        // The owner answered first: their answer stays. No row: nothing was ever asked.
        Ok(_)
        | Err(
            decisions::DecisionError::AlreadyDecided(_) | decisions::DecisionError::NotFound(_),
        ) => {}
        Err(error) => {
            tracing::warn!(job = %job.id, decision, %error, "could not answer the pending start's decision");
        }
    }
    drop_pending_start(pool, &job.id, name).await?;
    let line = format!(
        "{}: branch {name} admitted automatically, the disk is freed",
        job.goal
    );
    if let Err(error) = crate::feed::append(
        pool,
        job.project_id.as_deref(),
        "work_tree_pending",
        &line,
        None,
        None,
    )
    .await
    {
        tracing::warn!(job = %job.id, %error, "could not announce an admitted pending start");
    }
    Ok(())
}

/// PURE: whether `base` is a branch of the tree that has not merged. A base the tree does not know
/// does not hold a start back (as in `open_branch`).
fn base_still_unmerged(manifest: &Manifest, base: &str) -> bool {
    manifest
        .branches
        .get(base)
        .is_some_and(|branch| branch.state != "merged")
}

/// The base a pending start waits for was dropped, or is no longer in the tree.
fn base_is_gone(manifest: &Manifest, base: &str) -> bool {
    manifest
        .branches
        .get(base)
        .is_none_or(|branch| branch.state == "dropped")
}

/// PURE: whether a node in `status` can no longer take children. The tree's own root is exempt (its
/// dispatch report closes it as `done` while its starts may still wait), as in `find_or_open_node`.
fn parent_is_closed(status: &str, is_tree_root: bool) -> bool {
    !is_tree_root && ["done", "failed", "handoff", "blocked"].contains(&status)
}

/// PURE: whether a refused replay can never pass, whatever the disk, the slots or the other
/// branches do later: the request itself is wrong (400, 422), or the caller may not make it (403)
/// or names nothing there (404). A 409 or a 5xx may clear and is retried with the backoff.
fn refused_for_good(status: StatusCode) -> bool {
    matches!(
        status,
        StatusCode::BAD_REQUEST
            | StatusCode::FORBIDDEN
            | StatusCode::NOT_FOUND
            | StatusCode::UNPROCESSABLE_ENTITY
    )
}

/// The decision id that tells the owner a start of `name` was refused for good, fixed by job, name
/// and the record's generation, so a sweep repeated after a crash asks once.
fn refused_decision_id(job_id: &str, name: &str, generation: u64) -> String {
    format!("start-refused-{job_id}-{name}-{generation}")
}

/// Tells `caller`, the node that asked for the start of `name`, that the start was refused for good
/// and dropped: a later turn on its run when that run is live and listening (the conditions
/// `post_run_message` reads), else an open decision on that node, so the refusal waits in the
/// owner's inbox and the tree's status instead of nowhere. Best effort, logged.
async fn tell_caller(
    state: &AppState,
    job: &TreeJob,
    caller: &work_nodes::Node,
    name: &str,
    record: &Value,
    reason: &str,
) {
    let text = format!(
        "Tree {}: the start of branch {name} that node {} asked for will not be retried: {reason}. \
         The branch is dropped; start it again with corrected arguments if it is still needed.",
        job.id, caller.id
    );
    if deliver_to_run(state, caller.run_id.as_deref(), &text).await {
        return;
    }
    let generation = record["generation"].as_u64().unwrap_or(1);
    let id = refused_decision_id(&job.id, name, generation);
    if let Err(error) =
        decisions::ask_pending_with_id(&state.pool, &job.id, &caller.id, &text, &id).await
    {
        tracing::warn!(job = %job.id, branch = name, %error, "could not tell the caller of a refused start");
    }
}

/// Sends `text` as a later turn to run `run_id` when it is running and takes turns: steerable, not
/// a triage run, not toolless. False when it did not reach a listening run.
async fn deliver_to_run(state: &AppState, run_id: Option<&str>, text: &str) -> bool {
    let Some(run_id) = run_id.and_then(|id| id.parse::<i64>().ok()) else {
        return false;
    };
    let row: Option<(String, i64, String)> =
        match sqlx::query_as("SELECT status, steerable, mode FROM runs WHERE id = ?")
            .bind(run_id)
            .fetch_optional(&state.pool)
            .await
        {
            Ok(row) => row,
            Err(error) => {
                tracing::warn!(run_id, %error, "could not read the caller's run");
                return false;
            }
        };
    let Some((status, steerable, mode)) = row else {
        return false;
    };
    if status != "running"
        || steerable == 0
        || mode == crate::integrations::email::TRIAGE_MODE
        || crate::execution::runs::tool_policy_for_mode(&mode)
            == crate::execution::runner::ToolPolicy::None
    {
        return false;
    }
    let sender = state.run_messages.lock().unwrap().get(&run_id).cloned();
    let Some(sender) = sender else {
        return false;
    };
    sender
        .send(crate::execution::runner::LaterTurn {
            text: text.to_owned(),
            ..Default::default()
        })
        .is_ok()
}

/// First wait after a failed replay, doubled by each further failure.
const BACKOFF_BASE_SECS: i64 = 30;
/// The longest wait between two replays of one branch.
const BACKOFF_CAP_SECS: i64 = 3600;

/// PURE: seconds to wait after `attempts` failed replays: 30 s, 60 s, 120 s ... capped at an hour.
fn backoff_secs(attempts: u64) -> i64 {
    let doubled = BACKOFF_BASE_SECS.saturating_mul(1_i64 << attempts.saturating_sub(1).min(20));
    doubled.min(BACKOFF_CAP_SECS)
}

/// PURE: whether the record's backoff window is over. No (or an unreadable) stamp is due.
fn is_due(record: &Value) -> bool {
    record["next_attempt_at"]
        .as_str()
        .and_then(|at| chrono::DateTime::parse_from_rfc3339(at).ok())
        .is_none_or(|at| at <= chrono::Utc::now())
}

/// Counts one failed replay of `name` on its record and pushes `next_attempt_at` out by the
/// exponential backoff, so a start that cannot succeed is retried (and logged) ever more rarely.
pub(super) async fn record_failure(
    pool: &SqlitePool,
    job_id: &str,
    name: &str,
    error: &str,
) -> Result<(), String> {
    let lock = job_lock(job_id);
    let _guard = lock.lock().await;
    let Some(mut job) = store::load(pool, job_id).await.map_err(|e| e.to_string())? else {
        return Ok(());
    };
    let Some(record) = job
        .manifest
        .extra
        .get_mut("pending_starts")
        .and_then(Value::as_object_mut)
        .and_then(|pending| pending.get_mut(name))
        .and_then(Value::as_object_mut)
    else {
        return Ok(());
    };
    let attempts = record.get("attempts").and_then(Value::as_u64).unwrap_or(0) + 1;
    let at = chrono::Utc::now() + chrono::Duration::seconds(backoff_secs(attempts));
    record.insert("attempts".to_owned(), json!(attempts));
    record.insert(
        "next_attempt_at".to_owned(),
        json!(at.to_rfc3339_opts(chrono::SecondsFormat::Secs, false)),
    );
    record.insert("last_error".to_owned(), json!(error));
    persist(pool, &mut job).await
}

/// Fails the start of `name` for good: the branch is recorded as `dropped` with `reason` (a branch
/// the replay never added is added in that state, so the failure shows in the tree), the pending
/// decision is answered with the reason, and the record is consumed. One announcement.
async fn fail_pending_start(
    state: &AppState,
    job: &TreeJob,
    name: &str,
    record: &Value,
    reason: &str,
) -> Result<(), String> {
    drop_waiting_branch(&state.pool, &job.id, name, reason, Some(record)).await?;
    if let Some(decision) = record["decision_id"].as_str() {
        // The owner may have answered first (or nothing was asked): either way the answer stays.
        let _ = decisions::answer(&state.pool, decision, reason, "driver").await;
    }
    let line = format!("{}: branch {name} could not start, {reason}", job.goal);
    if let Err(error) = crate::feed::append(
        &state.pool,
        job.project_id.as_deref(),
        "work_tree_pending",
        &line,
        None,
        None,
    )
    .await
    {
        tracing::warn!(job = %job.id, %error, "could not announce a failed pending start");
    }
    Ok(())
}

/// Drops the branch `name` and its pending record, in one write under the job's lock, with `reason`
/// on the branch. A branch that was never added only loses its record, unless `add_from` (the
/// record) is given: then it is added as `dropped`, so the failure is visible.
async fn drop_waiting_branch(
    pool: &SqlitePool,
    job_id: &str,
    name: &str,
    reason: &str,
    add_from: Option<&Value>,
) -> Result<(), String> {
    let lock = job_lock(job_id);
    let _guard = lock.lock().await;
    let Some(mut job) = store::load(pool, job_id).await.map_err(|e| e.to_string())? else {
        return Ok(());
    };
    if !job.manifest.branches.contains_key(name)
        && let Some(record) = add_from
    {
        let paths: Vec<String> = record["paths"]
            .as_array()
            .map(|paths| {
                paths
                    .iter()
                    .filter_map(|path| path.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        let layer = record["layer"].as_str().unwrap_or("code");
        let mut branch = super::model::Branch::new(name, &paths, layer, record["base"].as_str());
        branch.state = "queued".to_owned();
        job.manifest.branches.insert(name.to_owned(), branch);
    }
    // An opened branch no node was ever bound to (a replay refused after its open) goes too.
    if let Some(branch) = job.manifest.branches.get_mut(name)
        && (branch.state == "queued" || (branch.state == "open" && branch.node_id.is_none()))
    {
        branch.state = "dropped".to_owned();
        branch.extra.insert("drop_reason".to_owned(), json!(reason));
        branch.extra.insert("dropped_at".to_owned(), json!(now()));
    }
    if let Some(pending) = job
        .manifest
        .extra
        .get_mut("pending_starts")
        .and_then(Value::as_object_mut)
    {
        pending.remove(name);
        if pending.is_empty() {
            job.manifest.extra.remove("pending_starts");
        }
    }
    let next = super::branch_start::current_generation(&job.manifest.extra) + 1;
    job.manifest
        .extra
        .insert(super::branch_start::GENERATION_KEY.to_owned(), json!(next));
    persist(pool, &mut job).await
}

/// Removes the pending record of `name` under the job's lock, from a fresh read of the manifest.
async fn drop_pending_start(pool: &SqlitePool, job_id: &str, name: &str) -> Result<(), String> {
    let lock = job_lock(job_id);
    let _guard = lock.lock().await;
    let Some(mut job) = store::load(pool, job_id).await.map_err(|e| e.to_string())? else {
        return Ok(());
    };
    let Some(pending) = job
        .manifest
        .extra
        .get_mut("pending_starts")
        .and_then(Value::as_object_mut)
    else {
        return Ok(());
    };
    if pending.remove(name).is_none() {
        return Ok(());
    }
    if pending.is_empty() {
        job.manifest.extra.remove("pending_starts");
    }
    // The record is consumed: the next refusal of a branch is a new question. Same write as the
    // drop, so a crash cannot leave one without the other.
    let next = super::branch_start::current_generation(&job.manifest.extra) + 1;
    job.manifest
        .extra
        .insert(super::branch_start::GENERATION_KEY.to_owned(), json!(next));
    persist(pool, &mut job).await
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    use super::*;
    use crate::execution::git_exec::testkit::{initialize_repo, space_free_tempdir};

    const TREE: &str = "pending-refused-tree";

    #[test]
    fn only_a_request_that_can_never_pass_is_refused_for_good() {
        for status in [
            StatusCode::BAD_REQUEST,
            StatusCode::FORBIDDEN,
            StatusCode::NOT_FOUND,
            StatusCode::UNPROCESSABLE_ENTITY,
        ] {
            assert!(refused_for_good(status), "{status}");
        }
        for status in [
            StatusCode::CONFLICT,
            StatusCode::SERVICE_UNAVAILABLE,
            StatusCode::INTERNAL_SERVER_ERROR,
        ] {
            assert!(!refused_for_good(status), "{status}");
        }
    }

    /// A tree job over a temp repository with a zero disk floor; answers the state, the repo's
    /// container (kept alive) and the root node id.
    async fn tree() -> (AppState, tempfile::TempDir, String) {
        let (state, runner) = crate::execution::runs::testkit::test_state_with_runner(
            Some(Duration::from_secs(5)),
            Duration::from_secs(600),
        )
        .await;
        runner.canned.lock().unwrap().as_mut().unwrap().session_id = None;
        let container = space_free_tempdir("pending-refused-");
        let path = container.path().join("repo");
        initialize_repo(&path);
        let dir = path.join(".nucleos/workflow");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("speed.yaml"), "tree:\n  floor_gb: 0\n").unwrap();
        let body = json!({
            "id": TREE,
            "goal": "Ship it",
            "feature_branch": "feat/pending-refused",
            "project_root": path.to_string_lossy().replace('\\', "/"),
            "project_id": "project-0",
        });
        let request = Request::builder()
            .method("POST")
            .uri("/trees")
            .header("Authorization", "Bearer test-token")
            .header("Content-Type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        let response = super::super::http::routes()
            .with_state(state.clone())
            .oneshot(request)
            .await
            .unwrap();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let created: Value = serde_json::from_slice(&bytes).unwrap();
        let root = created["root_node_id"].as_str().unwrap().to_owned();
        (state, container, root)
    }

    #[tokio::test]
    async fn a_pending_start_refused_with_a_400_is_dropped_and_its_caller_told() {
        let (state, _repo, root) = tree().await;
        // The caller has no live run to take a turn, so it is told through a decision.
        sqlx::query("UPDATE work_nodes SET run_id = NULL WHERE id = ?")
            .bind(&root)
            .execute(&state.pool)
            .await
            .unwrap();
        let mut job = store::load(&state.pool, TREE).await.unwrap().unwrap();
        job.manifest.extra.insert(
            "pending_starts".to_owned(),
            json!({ "classes-shell": {
                "generation": 1,
                "caller": root,
                "goal": "Build the shell classes",
                "prompt": "Do not run.",
                "paths": ["shell/**", "C:/Users/someone/.nucleos/projects/p/specs/x.md"],
                "layer": "core",
                "base": null,
                "waits_for_base": null,
                "kind": "leaf",
            }}),
        );
        store::update(&state.pool, &job).await.unwrap();

        retry_pending_starts(&state, TREE).await.unwrap();

        let job = store::load(&state.pool, TREE).await.unwrap().unwrap();
        assert!(
            !job.manifest.extra.contains_key("pending_starts"),
            "the record is consumed: {:?}",
            job.manifest.extra.get("pending_starts")
        );
        let branch = &job.manifest.branches["classes-shell"];
        assert_eq!(branch.state, "dropped");
        let reason = branch.extra["drop_reason"].as_str().unwrap();
        assert!(reason.contains("400"), "{reason}");
        assert!(reason.contains("state_dir"), "{reason}");
        let told: Option<(String, String, String)> =
            sqlx::query_as("SELECT node_id, question, status FROM decisions WHERE id = ?")
                .bind(refused_decision_id(TREE, "classes-shell", 1))
                .fetch_optional(&state.pool)
                .await
                .unwrap();
        let (node, question, status) = told.expect("the caller is told");
        assert_eq!(node, root);
        assert!(question.contains("classes-shell"), "{question}");
        assert_eq!(status, decisions::STATUS_OPEN);

        // A second sweep has nothing left to retry and asks nothing more.
        retry_pending_starts(&state, TREE).await.unwrap();
        let asked: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM decisions WHERE tree_id = ?")
            .bind(TREE)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(asked, 1);
    }
}
