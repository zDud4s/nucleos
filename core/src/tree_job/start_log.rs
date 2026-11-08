//! What `branch_start` leaves behind besides the branch: the shape a start was given (estimate,
//! descent, why a kind degraded) and every refusal, so that a tree can be explained afterwards.
//! There is no per-job event table, so both live in the job manifest: the shape on the branch's
//! `extra`, the refusals as a bounded list in the manifest's `extra`. No migration.

use std::collections::BTreeMap;

use serde_json::{Value, json};

use super::driver::persist;
use super::http::job_lock;
use super::partition::Kind;
use super::store;
use crate::state::AppState;

/// `manifest.extra` key holding the refusals of `tree_branch_start`, oldest first.
pub const REFUSALS_KEY: &str = "start_refusals";
/// The most refusals kept; the newest win.
pub const MAX_REFUSALS: usize = 50;
/// The feed kind of a descent (a new orchestrator, or a start degraded to a leaf).
pub const DESCENT_FEED_KIND: &str = "work_tree_descent";

/// Longest reason kept per refusal, in characters.
const MAX_REASON_CHARS: usize = 500;

/// How a start was decided, for the record.
pub struct StartShape<'a> {
    pub name: &'a str,
    pub kind: Kind,
    pub depth: i64,
    pub estimate: Option<i64>,
    pub leaf_budget: i64,
    /// The reason an orchestrator was opened as a leaf.
    pub degraded: Option<&'a str>,
    /// The caller left `kind` out: a leaf by default.
    pub kind_omitted: bool,
}

/// PURE: the `descent` record of a start, `None` for a plain leaf. An orchestrator descends with
/// the piece that made it one; a degraded start records why it did not.
pub fn descent(shape: &StartShape<'_>) -> Option<Value> {
    let reason = match (shape.degraded, shape.kind) {
        (Some(degraded), _) => degraded.to_owned(),
        (None, Kind::Orchestrator) => match shape.estimate {
            Some(estimate) => format!(
                "piece {} estimated {estimate} tokens > leaf_budget {}",
                shape.name, shape.leaf_budget
            ),
            None => format!(
                "piece {} asked to be an orchestrator with no estimate",
                shape.name
            ),
        },
        (None, Kind::Leaf) => return None,
    };
    Some(json!({
        "kind": shape.kind.as_str(),
        "estimate_tokens": shape.estimate,
        "leaf_budget": shape.leaf_budget,
        "depth": shape.depth,
        "reason": reason,
    }))
}

/// PURE: writes what `shape` says onto the branch's extras, never over what is there, so a replay
/// changes nothing. True when something was added.
pub fn stamp(extra: &mut BTreeMap<String, Value>, shape: &StartShape<'_>) -> bool {
    let mut changed = false;
    if let Some(estimate) = shape.estimate
        && !extra.contains_key("estimate_tokens")
    {
        extra.insert("estimate_tokens".to_owned(), json!(estimate));
        changed = true;
    }
    if !extra.contains_key("descent")
        && let Some(record) = descent(shape)
    {
        extra.insert("descent".to_owned(), record);
        changed = true;
    }
    if let Some(degraded) = shape.degraded
        && !extra.contains_key("degraded")
    {
        extra.insert("degraded".to_owned(), json!(degraded));
        changed = true;
    }
    if (shape.kind_omitted || shape.degraded.is_some()) && !extra.contains_key("kind_reason") {
        let reason = shape.degraded.map_or_else(
            || "kind omitted: opened as a leaf".to_owned(),
            str::to_owned,
        );
        extra.insert("kind_reason".to_owned(), json!(reason));
        changed = true;
    }
    changed
}

/// Records the shape of a start on its branch, once, and writes the descent's feed line the one
/// time the descent is first recorded. A failure is logged, never the request's.
pub async fn record_start(state: &AppState, job_id: &str, shape: &StartShape<'_>) {
    let lock = job_lock(job_id);
    let _guard = lock.lock().await;
    let mut job = match store::load(&state.pool, job_id).await {
        Ok(Some(job)) => job,
        Ok(None) => return,
        Err(error) => {
            tracing::warn!(%error, job = job_id, "could not read the job to record a start");
            return;
        }
    };
    let Some(branch) = job.manifest.branches.get_mut(shape.name) else {
        return;
    };
    let new_descent = !branch.extra.contains_key("descent") && descent(shape).is_some();
    if !stamp(&mut branch.extra, shape) {
        return;
    }
    if let Err(error) = persist(&state.pool, &mut job).await {
        tracing::warn!(%error, job = job_id, branch = shape.name, "could not record a start");
        return;
    }
    if new_descent && let Some(record) = descent(shape) {
        let line = format!(
            "{}: branch {} at depth {}: {}",
            job.goal,
            shape.name,
            shape.depth,
            record["reason"].as_str().unwrap_or_default()
        );
        if let Err(error) = crate::feed::append(
            &state.pool,
            job.project_id.as_deref(),
            DESCENT_FEED_KIND,
            &line,
            None,
            None,
        )
        .await
        {
            tracing::warn!(job = job_id, %error, "could not announce a descent");
        }
    }
}

/// PURE: appends `entry` to the refusals in `extra`, keeping the newest [`MAX_REFUSALS`]. An entry
/// equal to the last one (same branch, status and reason) is not added again: the driver replays a
/// refused start every sweep. True when the list changed.
pub fn push_refusal(extra: &mut BTreeMap<String, Value>, entry: Value) -> bool {
    let slot = extra
        .entry(REFUSALS_KEY.to_owned())
        .or_insert_with(|| json!([]));
    if !slot.is_array() {
        *slot = json!([]);
    }
    let Some(list) = slot.as_array_mut() else {
        return false;
    };
    let same = |a: &Value, b: &Value| {
        ["branch", "status", "reason"]
            .iter()
            .all(|key| a.get(*key) == b.get(*key))
    };
    if list.last().is_some_and(|last| same(last, &entry)) {
        return false;
    }
    list.push(entry);
    let excess = list.len().saturating_sub(MAX_REFUSALS);
    list.drain(..excess);
    true
}

/// Records one refusal of `tree_branch_start`: job, requested branch, status and reason. A failure
/// is logged, never the request's.
pub async fn record_refusal(
    state: &AppState,
    job_id: &str,
    branch: Option<&str>,
    status: u16,
    reason: &str,
) {
    let lock = job_lock(job_id);
    let _guard = lock.lock().await;
    let mut job = match store::load(&state.pool, job_id).await {
        Ok(Some(job)) => job,
        // A start for a job that does not exist has nowhere to be recorded.
        Ok(None) => return,
        Err(error) => {
            tracing::warn!(%error, job = job_id, "could not read the job to record a refusal");
            return;
        }
    };
    let reason: String = reason.chars().take(MAX_REASON_CHARS).collect();
    let entry = json!({
        "at": super::driver::now(),
        "job": job_id,
        "branch": branch,
        "status": status,
        "reason": reason,
    });
    if !push_refusal(&mut job.manifest.extra, entry) {
        return;
    }
    if let Err(error) = persist(&state.pool, &mut job).await {
        tracing::warn!(%error, job = job_id, "could not record a refusal");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shape<'a>(kind: Kind, estimate: Option<i64>, degraded: Option<&'a str>) -> StartShape<'a> {
        StartShape {
            name: "api-x",
            kind,
            depth: 2,
            estimate,
            leaf_budget: 120_000,
            degraded,
            kind_omitted: false,
        }
    }

    #[test]
    fn a_plain_leaf_has_no_descent_but_keeps_its_estimate() {
        let leaf = shape(Kind::Leaf, Some(50_000), None);
        assert_eq!(descent(&leaf), None);
        let mut extra = BTreeMap::new();
        assert!(stamp(&mut extra, &leaf));
        assert_eq!(extra["estimate_tokens"], 50_000);
        assert!(!extra.contains_key("descent"));
        assert!(!stamp(&mut extra, &leaf), "a replay changes nothing");
    }

    #[test]
    fn an_orchestrator_descends_with_the_piece_that_made_it_one() {
        let record = descent(&shape(Kind::Orchestrator, Some(180_000), None)).unwrap();
        assert_eq!(
            record["reason"],
            "piece api-x estimated 180000 tokens > leaf_budget 120000"
        );
        assert_eq!(record["kind"], "orchestrator");
        assert_eq!(record["depth"], 2);
        assert_eq!(record["leaf_budget"], 120_000);
    }

    #[test]
    fn a_degraded_start_records_why_on_the_branch() {
        let degraded = shape(Kind::Leaf, Some(900_000), Some("child repeats the goal"));
        let mut extra = BTreeMap::new();
        assert!(stamp(&mut extra, &degraded));
        assert_eq!(extra["degraded"], "child repeats the goal");
        assert_eq!(extra["kind_reason"], "child repeats the goal");
        assert_eq!(extra["descent"]["kind"], "leaf");
    }

    #[test]
    fn an_omitted_kind_leaves_its_reason() {
        let mut leaf = shape(Kind::Leaf, None, None);
        leaf.kind_omitted = true;
        let mut extra = BTreeMap::new();
        assert!(stamp(&mut extra, &leaf));
        assert_eq!(extra["kind_reason"], "kind omitted: opened as a leaf");
        assert!(!extra.contains_key("degraded"));
    }

    fn entry(branch: &str, reason: &str) -> Value {
        json!({ "branch": branch, "status": 409, "reason": reason })
    }

    #[test]
    fn the_refusal_list_keeps_the_newest_fifty_and_skips_a_repeat() {
        let mut extra = BTreeMap::new();
        assert!(push_refusal(&mut extra, entry("a", "r0")));
        assert!(!push_refusal(&mut extra, entry("a", "r0")), "a repeat");
        for n in 1..=60 {
            assert!(push_refusal(&mut extra, entry("a", &format!("r{n}"))));
        }
        let list = extra[REFUSALS_KEY].as_array().unwrap();
        assert_eq!(list.len(), MAX_REFUSALS);
        assert_eq!(list[0]["reason"], "r11");
        assert_eq!(list[MAX_REFUSALS - 1]["reason"], "r60");
    }
}
