//! What the driver does when a green branch does not merge (autonomy-trees spec B4).
//!
//! - A merge the queue refused for a passing cause (a lock, a fetch, the queue itself) is not a
//!   verdict on the branch: the branch stays `green`, waits a backoff of minutes on the ladder
//!   (`ladder::Kind::Merge`) and resubmits. Only past the ladder's retries is the owner asked, and
//!   the next answer is a fresh allowance.
//! - A conflict no resolver run claimed within the grace is not left to the owner either: the
//!   branch's own node is started again as a "resolve-conflict" run (key `resolve-<request>`), the
//!   branch goes back to `fixing`, and the usual gate and merge follow when that run reports `done`.
//!   Each failed merge request is one attempt (`conflict_attempts`); past the ladder's attempts the
//!   owner is asked.

use std::collections::BTreeMap;
use chrono::Utc;
use serde_json::{Value, json};
use sqlx::SqlitePool;

use super::driver::{Park, persist};
use super::engine::apply_transition;
use super::ladder::{self, Kind, Spent, Step};
use super::relaunch::{RESOLVE_PREFIX, budget_refusal, relaunch_node};
use super::retry::{is_load_reason, is_permanent_spawn_error};
use super::store::TreeJob;
use crate::state::AppState;

/// Transient refusals of a branch's merge in a row, on the branch's `extra`.
pub(super) const MERGE_RETRIES: &str = "merge_retries";
/// RFC 3339 instant before which the branch is not submitted again.
pub(super) const MERGE_BACKOFF_UNTIL: &str = "merge_backoff_until";

/// Whether a refusal reason names a passing cause rather than the branch: a lock, a fetch, the
/// queue. A conflict is never one, however it is worded around the cause.
pub(super) fn is_transient(reason: &str) -> bool {
    if is_permanent_spawn_error(reason) {
        return false;
    }
    let reason = reason.to_ascii_lowercase();
    if reason.contains("conflict") {
        return false;
    }
    [
        "index.lock",
        "unable to lock",
        "cannot lock",
        "could not lock",
        "lock file",
        "another git process",
        "database is locked",
        "fetch",
        "could not read from remote",
        "unable to access",
        "connection reset",
        "connection refused",
        "timed out",
        "timeout",
        "temporarily",
        "queue is busy",
        "queue busy",
    ]
    .iter()
    .any(|marker| reason.contains(marker))
        || is_load_reason(&reason)
}

/// Whether the branch is inside the backoff of a transient merge refusal.
pub(super) fn backing_off(extra: &BTreeMap<String, Value>) -> bool {
    extra
        .get(MERGE_BACKOFF_UNTIL)
        .and_then(Value::as_str)
        .and_then(|at| chrono::DateTime::parse_from_rfc3339(at).ok())
        .is_some_and(|until| until > Utc::now())
}

/// Forgets the transient streak of a branch. `true` when something was there.
pub(super) fn clear(job: &mut TreeJob, name: &str) -> bool {
    let Some(branch) = job.manifest.branches.get_mut(name) else {
        return false;
    };
    let had = branch.extra.remove(MERGE_RETRIES).is_some();
    branch.extra.remove(MERGE_BACKOFF_UNTIL).is_some() || had
}

/// The queue did not merge `name` (`settle_merge` already dropped the request, the branch is still
/// green). `claimed` is true when the driver itself claimed the conflict, which nothing but the
/// owner can settle: that and every reason that is not transient ask the owner at once.
pub(super) async fn on_not_merged(
    pool: &SqlitePool,
    job: &mut TreeJob,
    name: &str,
    node: Option<String>,
    reason: &str,
    claimed: bool,
    parks: &mut Vec<Park>,
) -> Result<(), String> {
    let Some(node) = node else {
        return Ok(());
    };
    if claimed || !is_transient(reason) {
        // Resubmitting a conflicted branch would conflict again: an owner decides.
        parks.push((node, format!("The merge of branch {name} failed: {reason}")));
        return Ok(());
    }
    let retries = job
        .manifest
        .branches
        .get(name)
        .and_then(|branch| branch.extra.get(MERGE_RETRIES))
        .and_then(Value::as_i64)
        .unwrap_or(0);
    // No agent can diagnose a lock: the rung after the retries is the park.
    match ladder::next_step(
        Kind::Merge,
        Spent {
            retries,
            ..Spent::default()
        },
    ) {
        Step::Retry { after } => {
            let until = Utc::now() + chrono::Duration::from_std(after).unwrap_or_default();
            if let Some(branch) = job.manifest.branches.get_mut(name) {
                branch.extra.insert(MERGE_RETRIES.to_owned(), json!(retries + 1));
                branch.extra.insert(
                    MERGE_BACKOFF_UNTIL.to_owned(),
                    json!(until.to_rfc3339_opts(chrono::SecondsFormat::Secs, false)),
                );
            }
            tracing::info!(job = %job.id, branch = name, %reason, retries = retries + 1, secs = after.as_secs(), "the merge failed for a passing cause: backing off");
            persist(pool, job).await
        }
        Step::Diagnose | Step::Park => {
            // The owner's answer is a fresh allowance.
            clear(job, name);
            persist(pool, job).await?;
            parks.push((
                node,
                format!(
                    "The merge of branch {name} failed {} times for a passing cause and backing off did not help: {reason}",
                    retries + 1
                ),
            ));
            Ok(())
        }
    }
}

/// The prompt suffix of a resolve-conflict run.
pub(super) fn resolve_suffix(integration: &str, reason: &str) -> String {
    format!(
        "\n\nYour branch conflicts with `{integration}`, and no conflict resolver took it. This is a resolve-conflict run: in your worktree merge `{integration}` into your branch, resolve every conflict keeping the intent of both sides, make sure the code still builds, commit, and finish the node again so the branch is gated and merged. What the queue said:\n{}",
        super::rescue::tail(reason, 1500)
    )
}

/// Starts the branch's node again as a resolve-conflict run, inside the tree. `Ok(true)` when it was
/// started (the branch is `fixing` again); `Ok(false)` when the ladder's attempts are spent, the
/// tree's budget refuses a paid run, or the node cannot be started: the caller asks the owner.
pub(super) async fn resolve_in_tree(
    state: &AppState,
    job: &mut TreeJob,
    name: &str,
    node: &str,
    reason: &str,
) -> Result<bool, String> {
    let pool = &state.pool;
    let Some(branch) = job.manifest.branches.get(name) else {
        return Ok(false);
    };
    let attempts = branch
        .extra
        .get("conflict_attempts")
        .and_then(Value::as_i64)
        .unwrap_or(1);
    let request = branch
        .extra
        .get("conflict_request_id")
        .and_then(Value::as_i64)
        .unwrap_or(attempts);
    // `attempts` counts merge requests, the first included: the retries are one fewer.
    let step = ladder::next_step(
        Kind::Conflict,
        Spent {
            retries: (attempts - 1).max(0),
            ..Spent::default()
        },
    );
    if !matches!(step, Step::Retry { .. }) || budget_refusal(pool, node).await.is_some() {
        return Ok(false);
    }
    let dead_run: Option<Option<String>> =
        sqlx::query_scalar("SELECT run_id FROM work_nodes WHERE id = ?")
            .bind(node)
            .fetch_optional(pool)
            .await
            .map_err(|e| e.to_string())?;
    let Some(dead_run) = dead_run.flatten() else {
        return Ok(false);
    };
    let integration = job
        .feature_branch
        .clone()
        .unwrap_or_else(|| "the integration branch".to_owned());
    let key = format!("{RESOLVE_PREFIX}{request}");
    let suffix = resolve_suffix(&integration, reason);
    if let Err(error) = relaunch_node(state, node, &dead_run, &key, Some(&suffix)).await {
        tracing::warn!(job = %job.id, branch = name, %error, "could not start the resolve-conflict run");
        return Ok(false);
    }
    let held = job
        .manifest
        .branches
        .get_mut(name)
        .ok_or("branch vanished")?;
    apply_transition(held, "fixing").map_err(|e| e.to_string())?;
    persist(pool, job).await?;
    let line = format!(
        "{}: branch {name} conflicts and no resolver picked it up; a resolve-conflict run was started in the tree (attempt {attempts})",
        job.goal
    );
    if let Err(error) = crate::feed::append(
        pool,
        job.project_id.as_deref(),
        "work_tree_conflict",
        &line,
        None,
        None,
    )
    .await
    {
        tracing::warn!(job = %job.id, %error, "could not announce the resolve-conflict run");
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locks_fetches_and_the_queue_are_transient() {
        for reason in [
            "fatal: Unable to create '.git/index.lock': File exists",
            "database is locked",
            "fetch failed: could not read from remote repository",
            "the queue is busy, try again",
            "operation timed out",
        ] {
            assert!(is_transient(reason), "{reason}");
        }
    }

    #[test]
    fn a_conflict_is_never_transient() {
        for reason in [
            "merge conflict in core/src/lib.rs",
            "CONFLICT (content): fetch of the base failed, conflict in a.rs",
            "the merge is not a fast-forward",
        ] {
            assert!(!is_transient(reason), "{reason}");
        }
    }

    #[test]
    fn a_permanent_spawn_error_is_not_transient() {
        assert!(!is_transient("failed to spawn git (os error 206)"));
    }

    #[test]
    fn the_backoff_window_is_read_from_the_branch() {
        let mut extra = BTreeMap::new();
        assert!(!backing_off(&extra));
        let later = (Utc::now() + chrono::Duration::minutes(5)).to_rfc3339();
        extra.insert(MERGE_BACKOFF_UNTIL.to_owned(), json!(later));
        assert!(backing_off(&extra));
        let past = (Utc::now() - chrono::Duration::minutes(5)).to_rfc3339();
        extra.insert(MERGE_BACKOFF_UNTIL.to_owned(), json!(past));
        assert!(!backing_off(&extra));
    }

    #[test]
    fn the_resolve_suffix_names_the_integration_branch_and_the_reason() {
        let suffix = resolve_suffix("feat/tree", "CONFLICT in a.rs");
        assert!(suffix.contains("`feat/tree`"), "{suffix}");
        assert!(suffix.contains("CONFLICT in a.rs"), "{suffix}");
        assert!(suffix.contains("resolve-conflict"), "{suffix}");
    }
}
