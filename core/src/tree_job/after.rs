//! A tree job that waits for another tree to land ("after", spec 2026-11-18-tree-launch-after).
//!
//! `POST /trees` with `after: <tree id>` stores the job with a mark in `manifest.extra["after"]`
//! (`{tree, since, reason}`) and starts no root run: the job stays `open`, holds no admission slot
//! and no worktree, and its feature branch is cut only when the root starts. The driver calls
//! `release` once per sweep, outside the job lock: when the predecessor has landed (or ended empty,
//! or held on the same feature branch) the mark is removed and the root starts the way `POST /trees`
//! starts it. A predecessor that can never satisfy the wait (abandoned, unknown, finished without a
//! landing) never starts the job and never cascades: the owner is asked once on the waiting root.

use chrono::{SecondsFormat, Utc};
use serde_json::{Value, json};

use super::decisions;
use super::http::job_lock;
use super::model::JOB_TERMINAL_STATE;
use super::store::{self, TreeJob};
use crate::state::AppState;

/// The key of the mark in `manifest.extra`.
pub const MARKER: &str = "after";

fn stamp() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

/// The mark a job waiting on `tree` carries.
pub fn mark_value(tree: &str, since: &str, reason: &str) -> Value {
    json!({ "tree": tree, "since": since, "reason": reason })
}

/// True while the job waits for its predecessor: it has the mark and is not finished (an abandoned
/// waiting job keeps a stale mark that must not count).
pub fn is_waiting(job: &TreeJob) -> bool {
    job.state != JOB_TERMINAL_STATE && job.manifest.extra.contains_key(MARKER)
}

/// The predecessor the job waits on, if it waits.
pub fn predecessor_of(job: &TreeJob) -> Option<String> {
    if !is_waiting(job) {
        return None;
    }
    job.manifest.extra[MARKER]["tree"]
        .as_str()
        .map(str::to_owned)
}

/// The `waiting_after` view (`{tree, since, reason}`) of a waiting job, `null` otherwise.
pub fn view(job: &TreeJob) -> Value {
    if is_waiting(job) {
        job.manifest.extra[MARKER].clone()
    } else {
        Value::Null
    }
}

/// What a predecessor's state means for the job that waits on it (spec D3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// The predecessor is done: the waiting job may start.
    Satisfied,
    /// Not yet: the reason is kept on the mark.
    Waiting(String),
    /// Never: the wait cannot end by itself, the owner decides.
    Refused(String),
}

/// Evaluates `pred` for a successor that will work on `successor_branch`.
pub fn evaluate(pred: &TreeJob, successor_branch: Option<&str>) -> Verdict {
    let extra = &pred.manifest.extra;
    if pred.state != JOB_TERMINAL_STATE {
        if let Some(refusal) = extra.get("land_refusal").and_then(Value::as_str) {
            return Verdict::Waiting(format!("the landing of {} was refused: {refusal}", pred.id));
        }
        if is_waiting(pred) {
            return Verdict::Waiting(format!("{} is itself waiting for another tree", pred.id));
        }
        return Verdict::Waiting(format!("{} has not landed yet", pred.id));
    }
    if extra
        .get("finish")
        .is_some_and(|finish| finish["status"] == json!("abandoned"))
    {
        return Verdict::Refused(format!("{} was abandoned", pred.id));
    }
    match extra.get("landing").and_then(Value::as_str) {
        Some("integration") => {
            if extra.get("land_sha").and_then(Value::as_str).is_some() {
                Verdict::Satisfied
            } else {
                Verdict::Refused(format!("{} finished without a landing", pred.id))
            }
        }
        Some("held") => {
            // Q3: an empty tree has nothing to wait for. Q2: held work is on its feature branch, so
            // only a successor on that same branch builds on it.
            let empty = extra.get("empty").and_then(Value::as_bool) == Some(true);
            let same_branch =
                successor_branch.is_some() && successor_branch == pred.feature_branch.as_deref();
            if empty || same_branch {
                Verdict::Satisfied
            } else {
                Verdict::Refused(format!(
                    "{} is held with work on {:?}, not on the branch of the waiting tree",
                    pred.id,
                    pred.feature_branch.as_deref().unwrap_or("")
                ))
            }
        }
        _ => Verdict::Refused(format!("{} finished without a landing", pred.id)),
    }
}

/// One driver step of a waiting job. Quiet while the predecessor has not landed; at the transition
/// to satisfied it starts the root (one feed line); when the wait can never end it asks the owner
/// once on the waiting root. Takes the job lock itself: the caller must not hold it.
pub async fn release(state: &AppState, job_id: &str) -> Result<(), String> {
    let Some(job) = store::load(&state.pool, job_id)
        .await
        .map_err(|e| e.to_string())?
    else {
        return Ok(());
    };
    let Some(pred_id) = predecessor_of(&job) else {
        return Ok(());
    };
    let pred = store::load(&state.pool, &pred_id)
        .await
        .map_err(|e| e.to_string())?;
    let verdict = match pred {
        Some(pred) => evaluate(&pred, job.feature_branch.as_deref()),
        None => Verdict::Refused(format!("tree {pred_id} does not exist")),
    };
    match verdict {
        Verdict::Waiting(reason) => note_reason(state, job_id, &reason).await,
        Verdict::Refused(reason) => {
            let question = format!(
                "Tree {job_id} waits for tree {pred_id}, which will never land: {reason}. \
                 Abandon this tree, or launch it again without `after`."
            );
            decisions::park_or_ask(&state.pool, job_id, &job.root_node_id, &question)
                .await
                .map(|_| ())
                .map_err(|e| e.to_string())
        }
        Verdict::Satisfied => start_root(state, &job, &pred_id).await,
    }
}

/// Starts the root of a released job, then removes the mark. Starting first means a crash in
/// between repeats the start (idempotent by its launch key) instead of leaving an open job with
/// no mark and no root.
async fn start_root(state: &AppState, job: &TreeJob, pred_id: &str) -> Result<(), String> {
    let started = super::root_pending::start(state, job).await;
    if let Err(reason) = &started {
        // `root_pending::start` marked the refusal: its sweep retry takes over from here.
        tracing::warn!(job = %job.id, %reason, "released tree job root run did not start");
    }
    {
        let lock = job_lock(&job.id);
        let _guard = lock.lock().await;
        let Some(mut stored) = store::load(&state.pool, &job.id)
            .await
            .map_err(|e| e.to_string())?
        else {
            return Ok(());
        };
        if stored.manifest.extra.remove(MARKER).is_none() {
            return Ok(());
        }
        stored.updated_at = stamp();
        store::update(&state.pool, &stored)
            .await
            .map_err(|e| e.to_string())?;
    }
    let line = format!(
        "tree job {}: {pred_id} landed, so the root run is released",
        job.id
    );
    let _ = crate::feed::append(
        &state.pool,
        None,
        "work_tree_after_released",
        &line,
        None,
        None,
    )
    .await;
    Ok(())
}

/// Keeps the latest reason on the mark, so `waiting_after.reason` answers why it still waits.
async fn note_reason(state: &AppState, job_id: &str, reason: &str) -> Result<(), String> {
    let lock = job_lock(job_id);
    let _guard = lock.lock().await;
    let Some(mut job) = store::load(&state.pool, job_id)
        .await
        .map_err(|e| e.to_string())?
    else {
        return Ok(());
    };
    if !is_waiting(&job) || job.manifest.extra[MARKER]["reason"].as_str() == Some(reason) {
        return Ok(());
    }
    if let Some(mark) = job.manifest.extra.get_mut(MARKER) {
        mark["reason"] = json!(reason);
    }
    job.updated_at = stamp();
    store::update(&state.pool, &job)
        .await
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree_job::model::Manifest;

    fn job(id: &str, branch: &str, state: &str) -> TreeJob {
        TreeJob {
            id: id.to_owned(),
            root_node_id: format!("{id}-root"),
            project_id: Some("project-0".to_owned()),
            feature_branch: Some(branch.to_owned()),
            manifest: Manifest::new(id, branch, "goal", "/repo", &[]),
            goal: "goal".to_owned(),
            state: state.to_owned(),
            contract: None,
            created_at: "t".to_owned(),
            updated_at: "t".to_owned(),
        }
    }

    fn finished(id: &str, branch: &str, extra: &[(&str, Value)]) -> TreeJob {
        let mut pred = job(id, branch, JOB_TERMINAL_STATE);
        for (key, value) in extra {
            pred.manifest.extra.insert((*key).to_owned(), value.clone());
        }
        pred
    }

    #[test]
    fn a_landed_predecessor_satisfies() {
        let pred = finished(
            "a",
            "feat/a",
            &[
                ("landing", json!("integration")),
                ("land_sha", json!("abc")),
            ],
        );
        assert_eq!(evaluate(&pred, Some("feat/b")), Verdict::Satisfied);
    }

    #[test]
    fn an_integration_finish_without_a_sha_is_refused() {
        let pred = finished("a", "feat/a", &[("landing", json!("integration"))]);
        assert!(matches!(evaluate(&pred, None), Verdict::Refused(_)));
    }

    #[test]
    fn an_empty_held_predecessor_satisfies() {
        let pred = finished(
            "a",
            "feat/a",
            &[("landing", json!("held")), ("empty", json!(true))],
        );
        assert_eq!(evaluate(&pred, Some("feat/b")), Verdict::Satisfied);
    }

    #[test]
    fn a_held_predecessor_with_work_satisfies_only_on_the_same_branch() {
        let pred = finished("a", "feat/a", &[("landing", json!("held"))]);
        assert_eq!(evaluate(&pred, Some("feat/a")), Verdict::Satisfied);
        assert!(matches!(
            evaluate(&pred, Some("feat/b")),
            Verdict::Refused(_)
        ));
    }

    #[test]
    fn an_abandoned_predecessor_is_refused() {
        let pred = finished(
            "a",
            "feat/a",
            &[("finish", json!({"status": "abandoned", "reason": "x"}))],
        );
        assert!(matches!(
            evaluate(&pred, Some("feat/a")),
            Verdict::Refused(_)
        ));
    }

    #[test]
    fn an_open_predecessor_waits_and_keeps_the_land_refusal() {
        let mut pred = job("a", "feat/a", "open");
        assert!(matches!(evaluate(&pred, None), Verdict::Waiting(_)));
        pred.manifest
            .extra
            .insert("land_refusal".to_owned(), json!("the gate is red"));
        match evaluate(&pred, None) {
            Verdict::Waiting(reason) => assert!(reason.contains("the gate is red"), "{reason}"),
            other => panic!("expected Waiting, got {other:?}"),
        }
    }

    #[test]
    fn a_finished_job_is_not_waiting_even_with_a_stale_mark() {
        let mut waiting = job("b", "feat/b", "open");
        waiting
            .manifest
            .extra
            .insert(MARKER.to_owned(), mark_value("a", "t", "r"));
        assert!(is_waiting(&waiting));
        assert_eq!(predecessor_of(&waiting).as_deref(), Some("a"));
        waiting.state = JOB_TERMINAL_STATE.to_owned();
        assert!(!is_waiting(&waiting));
        assert_eq!(view(&waiting), Value::Null);
    }
}
