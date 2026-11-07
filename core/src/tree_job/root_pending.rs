//! A tree job whose root run has not started yet ("root pending").
//!
//! `POST /trees` stores the job and then starts the root run. When that start is refused (the disk
//! floor, a git failure), the job stays `open` and carries a marker in `manifest.extra` with the
//! reason. The driver retries the start on each sweep until it works; the marker is then cleared.

use chrono::{SecondsFormat, Utc};
use serde_json::json;

use super::http::job_lock;
use super::model::JOB_TERMINAL_STATE;
use super::store::{self, TreeJob};
use crate::state::AppState;

/// The key of the marker in `manifest.extra`.
pub const MARKER: &str = "root_start";

/// Why the root run has not started, if the job is waiting for it.
pub fn reason_of(job: &TreeJob) -> Option<String> {
    job.manifest
        .extra
        .get(MARKER)
        .and_then(|marker| marker["reason"].as_str())
        .map(str::to_owned)
}

fn stamp() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

/// Starts the root run. A refusal is recorded on the job (the earliest `since` is kept) and
/// returned; a success clears an earlier mark. A failed marker write never masks the result.
/// Takes the job lock itself, after `root::start` is done: the caller must not hold it.
pub async fn start(state: &AppState, job: &TreeJob) -> Result<i64, String> {
    let result = super::root::start(state, job).await;
    let marked = match &result {
        Ok(_) => clear(state, &job.id).await,
        Err(reason) => mark(state, &job.id, reason).await,
    };
    if let Err(error) = marked {
        tracing::warn!(job = %job.id, %error, "tree job root start marker was not updated");
    }
    result
}

async fn mark(state: &AppState, job_id: &str, reason: &str) -> Result<(), String> {
    let lock = job_lock(job_id);
    let _guard = lock.lock().await;
    let Some(mut job) = store::load(&state.pool, job_id)
        .await
        .map_err(|e| e.to_string())?
    else {
        return Ok(());
    };
    let now = stamp();
    let since = job
        .manifest
        .extra
        .get(MARKER)
        .and_then(|marker| marker["since"].as_str())
        .map_or_else(|| now.clone(), str::to_owned);
    job.manifest.extra.insert(
        MARKER.to_owned(),
        json!({ "reason": reason, "since": since, "at": now }),
    );
    job.updated_at = now;
    store::update(&state.pool, &job)
        .await
        .map_err(|e| e.to_string())
}

async fn clear(state: &AppState, job_id: &str) -> Result<(), String> {
    let lock = job_lock(job_id);
    let _guard = lock.lock().await;
    let Some(mut job) = store::load(&state.pool, job_id)
        .await
        .map_err(|e| e.to_string())?
    else {
        return Ok(());
    };
    if job.manifest.extra.remove(MARKER).is_none() {
        return Ok(());
    }
    job.updated_at = stamp();
    store::update(&state.pool, &job)
        .await
        .map_err(|e| e.to_string())
}

/// One driver retry of a pending root start. Quiet while it is still refused: the reason is
/// already on the job, so nothing is announced on every sweep.
pub async fn retry(state: &AppState, job_id: &str) -> Result<(), String> {
    let Some(job) = store::load(&state.pool, job_id)
        .await
        .map_err(|e| e.to_string())?
    else {
        return Ok(());
    };
    if job.state == JOB_TERMINAL_STATE || reason_of(&job).is_none() {
        return Ok(());
    }
    match start(state, &job).await {
        Ok(_) => {
            let line = format!(
                "tree job {}: root run started after an earlier refusal",
                job.id
            );
            let _ = crate::feed::append(
                &state.pool,
                None,
                "work_tree_root_started",
                &line,
                None,
                None,
            )
            .await;
        }
        Err(reason) => {
            tracing::warn!(job = %job_id, %reason, "tree job root run still not started");
        }
    }
    Ok(())
}
