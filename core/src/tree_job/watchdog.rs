//! Tree-job node watchdog: a running node always has a session, or somebody is asked why not.
//!
//! `wrap` relaunches a node whose run died and `root` retries a root whose start failed, but a
//! branch node bound to its branch whose launch never produced a run (the branch was not admitted,
//! the daemon stopped between the two steps) had no run for either to look at and stayed `running`
//! forever. A run whose row says live while nothing has moved for hours was never noticed either.
//! This module closes both gaps and counts the relaunches that fail:
//!
//! - an orphan (a running branch node with no run) is started again from its stored launch;
//! - a live run whose lease (its last sign of life) is older than [`LEASE_TTL_SECS`] is ended as
//!   `interrupted`, so `wrap` starts the node again under its `died-` chain;
//! - a relaunch that fails (here or in `wrap`) sets a backoff (60 s, doubling, at most 30 min) during
//!   which the node is left alone, and the [`FAILURE_CAP`]th failure in a row parks the branch with
//!   an owner decision (`decisions::park_or_ask`). Its answer is a fresh allowance.

use chrono::{DateTime, Duration, Utc};
use serde_json::Value;
use sqlx::SqlitePool;

use crate::execution::runs::finalize_termination;
use crate::state::AppState;

use super::decisions;
use super::engine::branch_node_may_close;
use super::model::JOB_TERMINAL_STATE;
use super::open::{latest_launch, relaunch_node, run_is_live};
use super::relaunch::{budget_refusal, refusal_question};
use super::store;

/// Failed relaunches in a row after which the owner is asked.
pub(super) const FAILURE_CAP: i64 = 2;
/// A live run with no sign of life for this long has lost its session.
pub(super) const LEASE_TTL_SECS: i64 = 2 * 60 * 60;
const FIRST_BACKOFF_SECS: i64 = 60;
const MAX_BACKOFF_SECS: i64 = 30 * 60;
const PARKED: &str = "parked";

/// The wait after `failures` failed relaunches in a row.
pub(super) fn backoff(failures: i64) -> Duration {
    let doubled = FIRST_BACKOFF_SECS.saturating_mul(1_i64 << (failures - 1).clamp(0, 20));
    Duration::seconds(doubled.min(MAX_BACKOFF_SECS))
}

/// Whether a failed root start recorded in `manifest.extra` (`root::ROOT_START_KEY`) must still wait
/// before its next try. Only an admission refusal waits (the cause, a full disk say, does not go
/// away in a tick); any other failure is retried at once, as before.
pub(super) fn root_start_waits(record: &Value) -> bool {
    let refused = record["error"]
        .as_str()
        .is_some_and(|error| error.contains("not admitted"));
    let attempts = record["attempts"].as_u64().unwrap_or(0) as i64;
    let Some(at) = record["at"]
        .as_str()
        .and_then(|at| DateTime::parse_from_rfc3339(at).ok())
    else {
        return false;
    };
    refused && attempts > 0 && Utc::now() < at.with_timezone(&Utc) + backoff(attempts)
}

/// Marks `node_id` alive now: its lease is renewed.
pub async fn beat(pool: &SqlitePool, node_id: &str) -> Result<(), sqlx::Error> {
    let now = Utc::now().to_rfc3339();
    sqlx::query(
        "INSERT INTO node_watchdog (node_id, beat_at, updated_at) VALUES (?, ?, ?) \
         ON CONFLICT (node_id) DO UPDATE SET beat_at = excluded.beat_at, \
                                             updated_at = excluded.updated_at",
    )
    .bind(node_id)
    .bind(&now)
    .bind(&now)
    .execute(pool)
    .await
    .map(|_| ())
}

/// The latest sign of life of `node_id`: its renewed lease, a transition it wrote, a change of its
/// row, or the start of its run.
async fn last_beat(pool: &SqlitePool, node_id: &str, run_id: &str) -> Option<DateTime<Utc>> {
    let rows: Vec<(String,)> = sqlx::query_as(
        "SELECT beat_at FROM node_watchdog WHERE node_id = ? AND beat_at IS NOT NULL \
         UNION ALL SELECT created_at FROM tree_transitions WHERE node_id = ? \
         UNION ALL SELECT updated_at FROM work_nodes WHERE id = ? \
         UNION ALL SELECT created_at FROM runs WHERE id = CAST(? AS INTEGER)",
    )
    .bind(node_id)
    .bind(node_id)
    .bind(node_id)
    .bind(run_id)
    .fetch_all(pool)
    .await
    .ok()?;
    rows.iter()
        .filter_map(|(at,)| DateTime::parse_from_rfc3339(at).ok())
        .map(|at| at.with_timezone(&Utc))
        .max()
}

type Record = (i64, Option<String>, Option<String>);

async fn record_of(pool: &SqlitePool, node_id: &str) -> Result<Option<Record>, String> {
    sqlx::query_as("SELECT failures, next_at, decision_id FROM node_watchdog WHERE node_id = ?")
        .bind(node_id)
        .fetch_optional(pool)
        .await
        .map_err(|e| e.to_string())
}

/// Whether `node_id` is inside the backoff of a failed relaunch.
pub(super) async fn backing_off(pool: &SqlitePool, node_id: &str) -> bool {
    let Ok(Some((_, Some(next_at), _))) = record_of(pool, node_id).await else {
        return false;
    };
    DateTime::parse_from_rfc3339(&next_at).is_ok_and(|at| Utc::now() < at.with_timezone(&Utc))
}

/// Forgets the failures of `node_id`: its relaunch worked.
pub(super) async fn record_success(pool: &SqlitePool, node_id: &str) {
    let result =
        sqlx::query("UPDATE node_watchdog SET failures = 0, next_at = NULL, last_error = NULL, decision_id = NULL WHERE node_id = ?")
            .bind(node_id)
            .execute(pool)
            .await;
    if let Err(error) = result {
        tracing::warn!(node = node_id, %error, "watchdog could not clear the failures");
    }
}

/// Drops the failures of a node whose parking decision was answered: a fresh allowance.
async fn reset_if_answered(pool: &SqlitePool, node_id: &str) -> Result<(), String> {
    let Some((_, _, Some(decision_id))) = record_of(pool, node_id).await? else {
        return Ok(());
    };
    let status: Option<String> = sqlx::query_scalar("SELECT status FROM decisions WHERE id = ?")
        .bind(&decision_id)
        .fetch_optional(pool)
        .await
        .map_err(|e| e.to_string())?;
    if status.as_deref() != Some(decisions::STATUS_OPEN) {
        record_success(pool, node_id).await;
    }
    Ok(())
}

/// Counts one more failed relaunch of `node_id`, sets the backoff, and at [`FAILURE_CAP`] parks the
/// branch (or asks on the root) with a decision that carries the last error.
pub(super) async fn record_failure(
    pool: &SqlitePool,
    job_id: &str,
    node_id: &str,
    error: &str,
) -> Result<(), String> {
    let now = Utc::now();
    let failures = record_of(pool, node_id)
        .await?
        .map_or(0, |(failures, _, _)| failures)
        + 1;
    let next_at = (now + backoff(failures)).to_rfc3339();
    sqlx::query(
        "INSERT INTO node_watchdog (node_id, failures, next_at, last_error, updated_at) \
         VALUES (?, ?, ?, ?, ?) \
         ON CONFLICT (node_id) DO UPDATE SET failures = excluded.failures, \
             next_at = excluded.next_at, last_error = excluded.last_error, \
             updated_at = excluded.updated_at",
    )
    .bind(node_id)
    .bind(failures)
    .bind(&next_at)
    .bind(error)
    .bind(now.to_rfc3339())
    .execute(pool)
    .await
    .map_err(|e| e.to_string())?;
    if failures < FAILURE_CAP {
        return Ok(());
    }
    let question = format!(
        "Node {node_id} could not be started again {failures} times in a row (last error: \
         {error}). Fix the cause and answer to try again, or say how the branch should go on."
    );
    let asked = decisions::park_or_ask(pool, job_id, node_id, &question)
        .await
        .map_err(|e| e.to_string())?;
    if let Some(asked) = asked {
        sqlx::query("UPDATE node_watchdog SET decision_id = ? WHERE node_id = ?")
            .bind(asked.id)
            .bind(node_id)
            .execute(pool)
            .await
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// One pass over the running branch nodes of unfinished jobs. A failure on one node is logged and
/// stops no other. Safe to call again at any time.
pub async fn sweep(state: &AppState) {
    let rows: Result<Vec<(String, String)>, _> = sqlx::query_as(
        "SELECT n.id, n.tree_job_id FROM work_nodes n JOIN tree_jobs j ON j.id = n.tree_job_id \
         WHERE n.status = 'running' AND j.state <> ? ORDER BY n.created_at, n.id",
    )
    .bind(JOB_TERMINAL_STATE)
    .fetch_all(&state.pool)
    .await;
    let rows = match rows {
        Ok(rows) => rows,
        Err(error) => {
            tracing::warn!(%error, "watchdog could not list the running nodes");
            return;
        }
    };
    for (node_id, job_id) in rows {
        if let Err(error) = check_node(state, &node_id, &job_id).await {
            tracing::warn!(node = %node_id, %error, "watchdog failed for the node");
        }
    }
}

async fn check_node(state: &AppState, node_id: &str, job_id: &str) -> Result<(), String> {
    let pool = &state.pool;
    reset_if_answered(pool, node_id).await?;
    let Some(job) = store::load(pool, job_id).await.map_err(|e| e.to_string())? else {
        return Ok(());
    };
    // The root is looked after by `wrap` and `root::retry_failed_starts`.
    let Some(branch) = job
        .manifest
        .branches
        .values()
        .find(|branch| branch.node_id.as_deref() == Some(node_id))
    else {
        return Ok(());
    };
    if job.state == JOB_TERMINAL_STATE || branch.state == PARKED || branch_node_may_close(branch) {
        return Ok(());
    }
    let waiting: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM decisions WHERE node_id = ? AND status = ?")
            .bind(node_id)
            .bind(decisions::STATUS_OPEN)
            .fetch_one(pool)
            .await
            .map_err(|e| e.to_string())?;
    if waiting > 0 || backing_off(pool, node_id).await {
        return Ok(());
    }
    // Read again: `wrap` or the boot relaunch may have started the node since the list was made.
    let run_id: Option<Option<String>> =
        sqlx::query_scalar("SELECT run_id FROM work_nodes WHERE id = ? AND status = 'running'")
            .bind(node_id)
            .fetch_optional(pool)
            .await
            .map_err(|e| e.to_string())?;
    match run_id {
        None => Ok(()),
        Some(None) => relaunch_orphan(state, job_id, node_id).await,
        Some(Some(run_id)) => {
            if !run_is_live(pool, &run_id).await.map_err(|e| e.to_string())? {
                return Ok(()); // a dead run is `wrap`'s
            }
            let expired = last_beat(pool, node_id, &run_id)
                .await
                .is_some_and(|at| Utc::now() > at + Duration::seconds(LEASE_TTL_SECS));
            if expired {
                if let Ok(number) = run_id.parse::<i64>() {
                    // Ending it makes it a run that died without a report: `wrap` starts the node again.
                    finalize_termination(state, number, "interrupted").await;
                }
            }
            Ok(())
        }
    }
}

/// Starts a running branch node that has no run from its latest stored launch.
async fn relaunch_orphan(state: &AppState, job_id: &str, node_id: &str) -> Result<(), String> {
    let pool = &state.pool;
    if let Some(reason) = budget_refusal(pool, node_id).await {
        let question = refusal_question(node_id, &reason);
        decisions::park_or_ask(pool, job_id, node_id, &question)
            .await
            .map_err(|e| e.to_string())?;
        return Ok(());
    }
    let failures = record_of(pool, node_id)
        .await?
        .map_or(0, |(failures, _, _)| failures);
    let outcome = match latest_launch(pool, node_id).await {
        Ok(Some(_)) => {
            relaunch_node(state, node_id, "none", &format!("orphan-{failures}"), None)
                .await
                .map(|_| ())
        }
        Ok(None) => Err("the node has no run and no launch was stored".to_owned()),
        Err(error) => Err(error.to_string()),
    };
    match outcome {
        Ok(()) => {
            record_success(pool, node_id).await;
            Ok(())
        }
        Err(error) => record_failure(pool, job_id, node_id, &error).await,
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration as StdDuration;

    use super::*;
    use crate::tree_job::model::{Branch, Manifest};
    use crate::tree_job::open::{self, StoredLaunch};
    use crate::tree_job::store::TreeJob;

    const JOB: &str = "wd-job";
    const NODE: &str = "wd-node";
    const OLD: &str = "2026-10-31T00:00:00+00:00";

    async fn state() -> AppState {
        let (state, runner) = crate::execution::runs::testkit::test_state_with_runner(
            Some(StdDuration::from_secs(60)),
            StdDuration::from_secs(600),
        )
        .await;
        runner.canned.lock().unwrap().as_mut().unwrap().session_id = None;
        state
    }

    async fn seed_node(state: &AppState, run_id: Option<i64>) {
        let mut manifest = Manifest::new(JOB, "feat/wd", "Watch the nodes", "C:/nowhere", &[]);
        manifest.state = "open".to_owned();
        let mut branch = Branch::new("branch", &["core/src/**".to_owned()], "core", None);
        branch.state = "building".to_owned();
        branch.node_id = Some(NODE.to_owned());
        manifest.branches.insert(branch.name.clone(), branch);
        store::save(
            &state.pool,
            &TreeJob {
                id: JOB.to_owned(),
                root_node_id: "wd-root".to_owned(),
                project_id: None,
                feature_branch: Some("feat/wd".to_owned()),
                goal: "Watch the nodes".to_owned(),
                state: "open".to_owned(),
                manifest,
                contract: None,
                created_at: OLD.to_owned(),
                updated_at: OLD.to_owned(),
            },
        )
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO work_nodes (id, root_id, depth, kind, title, goal, status, run_id, \
                                     tree_job_id, handoff_path, created_at, updated_at) \
             VALUES (?, 'wd-root', 0, 'orchestrator', 't', 'g', 'running', ?, ?, 'C:/h.md', ?, ?)",
        )
        .bind(NODE)
        .bind(run_id.map(|id| id.to_string()))
        .bind(JOB)
        .bind(OLD)
        .bind(OLD)
        .execute(&state.pool)
        .await
        .unwrap();
    }

    async fn seed_launch(state: &AppState) {
        open::save_launch(
            &state.pool,
            &StoredLaunch {
                node_id: NODE.to_owned(),
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

    async fn node_run(state: &AppState) -> Option<String> {
        sqlx::query_scalar("SELECT run_id FROM work_nodes WHERE id = ?")
            .bind(NODE)
            .fetch_one(&state.pool)
            .await
            .unwrap()
    }

    async fn failures(state: &AppState) -> i64 {
        sqlx::query_scalar("SELECT failures FROM node_watchdog WHERE node_id = ?")
            .bind(NODE)
            .fetch_optional(&state.pool)
            .await
            .unwrap()
            .unwrap_or(0)
    }

    async fn past_backoff(state: &AppState) {
        sqlx::query("UPDATE node_watchdog SET next_at = ? WHERE node_id = ?")
            .bind(OLD)
            .bind(NODE)
            .execute(&state.pool)
            .await
            .unwrap();
    }

    async fn branch_state(state: &AppState) -> String {
        store::load(&state.pool, JOB).await.unwrap().unwrap().manifest.branches["branch"]
            .state
            .clone()
    }

    #[tokio::test]
    async fn a_running_branch_node_with_no_run_is_relaunched_from_its_stored_launch() {
        let state = state().await;
        seed_node(&state, None).await;
        seed_launch(&state).await;

        sweep(&state).await;

        assert!(node_run(&state).await.is_some());
        assert_eq!(failures(&state).await, 0);
    }

    #[tokio::test]
    async fn a_failed_relaunch_is_not_tried_again_before_its_backoff_passes() {
        let state = state().await;
        seed_node(&state, None).await; // no launch stored: the relaunch fails

        sweep(&state).await;
        sweep(&state).await;

        assert_eq!(failures(&state).await, 1);
        assert!(node_run(&state).await.is_none());
    }

    #[tokio::test]
    async fn two_failed_relaunches_in_a_row_park_the_branch_with_an_open_decision() {
        let state = state().await;
        seed_node(&state, None).await;

        sweep(&state).await;
        past_backoff(&state).await;
        sweep(&state).await;

        assert_eq!(failures(&state).await, 2);
        assert_eq!(branch_state(&state).await, "parked");
        let open: (String, String) = sqlx::query_as(
            "SELECT node_id, question FROM decisions WHERE status = 'open' AND tree_id = ?",
        )
        .bind(JOB)
        .fetch_one(&state.pool)
        .await
        .unwrap();
        assert_eq!(open.0, NODE);
        assert!(open.1.contains("no launch was stored"), "{}", open.1);
    }

    #[tokio::test]
    async fn an_answered_decision_gives_the_node_a_fresh_allowance() {
        let state = state().await;
        seed_node(&state, None).await;
        sweep(&state).await;
        past_backoff(&state).await;
        sweep(&state).await;
        sqlx::query("UPDATE decisions SET status = 'answered', answer = 'retry' WHERE node_id = ?")
            .bind(NODE)
            .execute(&state.pool)
            .await
            .unwrap();

        sweep(&state).await;

        assert_eq!(failures(&state).await, 0);
    }

    #[tokio::test]
    async fn a_live_run_with_an_expired_lease_is_ended_and_a_fresh_one_is_kept() {
        let state = state().await;
        seed_node(&state, Some(51)).await;
        sqlx::query("INSERT INTO runs (id, prompt, status, created_at) VALUES (51, 'p', 'running', ?)")
            .bind(OLD)
            .execute(&state.pool)
            .await
            .unwrap();
        beat(&state.pool, NODE).await.unwrap();

        sweep(&state).await;
        let status = |state: &AppState| {
            let pool = state.pool.clone();
            async move {
                sqlx::query_scalar::<_, String>("SELECT status FROM runs WHERE id = 51")
                    .fetch_one(&pool)
                    .await
                    .unwrap()
            }
        };
        assert_eq!(status(&state).await, "running");

        sqlx::query("UPDATE node_watchdog SET beat_at = ? WHERE node_id = ?")
            .bind(OLD)
            .bind(NODE)
            .execute(&state.pool)
            .await
            .unwrap();
        sweep(&state).await;

        assert_ne!(status(&state).await, "running");
    }

    #[test]
    fn the_backoff_doubles_from_a_minute_up_to_half_an_hour() {
        assert_eq!(backoff(1), Duration::seconds(60));
        assert_eq!(backoff(2), Duration::seconds(120));
        assert_eq!(backoff(30), Duration::seconds(1800));
    }
}
