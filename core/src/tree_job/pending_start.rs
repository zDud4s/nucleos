//! Pending starts: replays the branch starts that `branch_start` stored because the disk alone
//! refused them, each sweep of the driver, once the disk (or the base branch it waits for) admits
//! them. A record is consumed, never left behind: a crash between any two writes repeats safely.

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
    super::branch_start::start_pending(state, &job.id, name, record).await?;

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
    if let Some(branch) = job.manifest.branches.get_mut(name)
        && branch.state == "queued"
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
