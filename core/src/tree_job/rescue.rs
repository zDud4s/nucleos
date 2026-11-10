//! One bounded autonomous rescue before the driver asks the owner.
//!
//! A branch is parked when its node ended `blocked` or `failed`, or when its gate stayed red after
//! the ladder's fix retries (`ladder::Kind::Fix`), or when a chain of relaunches ran out of
//! retries (`wrap`). Before that question goes out, the node is started once more with the evidence
//! of the failure in its prompt (key `rescue-<dead run>`): this is the ladder's "diagnose and
//! decide" rung. The allowance is counted in `tree_launches`, the way fixes are: one rescue per
//! node since the owner's last answer (`decision-<id>` launch), so an answer is a fresh allowance
//! and a rescue that fails too falls through to the park as before.

use std::path::Path;

use sqlx::SqlitePool;

use super::ladder::{Kind, Spent, Step, next_step};
use super::relaunch::{RESCUE_PREFIX, budget_refusal, relaunch_node};
use crate::state::AppState;
use crate::work_nodes;

/// Longest tail of a failure's evidence (a gate's findings, a handoff) that goes into the prompt.
const EVIDENCE_MAX: usize = 3000;

/// The last `max` bytes of `text`, cut on a character boundary.
pub(super) fn tail(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut start = text.len() - max;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    &text[start..]
}

/// The prompt suffix for a node whose run ended `blocked` or `failed`: what stopped it is its own
/// last handoff, when one was written.
pub(super) fn stopped_suffix(outcome: &str, handoff: Option<&str>) -> String {
    let mut suffix = format!(
        "\n\nYour previous run ended {outcome}. This is the one automatic rescue run before the owner is asked: find what stopped you, fix the cause and finish the node again."
    );
    if let Some(handoff) = handoff.map(str::trim).filter(|text| !text.is_empty()) {
        suffix.push_str("\n\nYour last handoff said:\n");
        suffix.push_str(tail(handoff, EVIDENCE_MAX));
    }
    suffix
}

/// The prompt suffix for a branch whose gate stayed red past the fix cap: the usual fix suffix
/// (`fix`, the findings and what to do with them) and the warning that this is the last try.
pub(super) fn red_suffix(fix: &str) -> String {
    format!(
        "{fix}\n\nThis is the one automatic rescue run left: the gate stayed red after the automatic fixes, so look for the root cause rather than patching the last finding."
    )
}

/// The text of the node's handoff file, when it has one that can be read.
pub(super) async fn handoff_text(pool: &SqlitePool, node_id: &str) -> Option<String> {
    let node = work_nodes::get(pool, node_id).await.ok()?;
    let path = node.handoff_path.filter(|path| Path::new(path).is_file())?;
    tokio::fs::read_to_string(path).await.ok()
}

/// Rescues spent by the node since the owner's last answer, not counting the one under `key`
/// (a replay of the same rescue is not a prior one).
async fn spent(pool: &SqlitePool, node_id: &str, key: &str) -> Result<i64, String> {
    sqlx::query_scalar(
        "SELECT COUNT(*) FROM tree_launches \
         WHERE node_id = ? AND key LIKE ? AND key <> ? \
           AND seq > COALESCE((SELECT MAX(seq) FROM tree_launches \
                               WHERE node_id = ? AND key LIKE 'decision-%'), 0)",
    )
    .bind(node_id)
    .bind(format!("{RESCUE_PREFIX}%"))
    .bind(key)
    .bind(node_id)
    .fetch_one(pool)
    .await
    .map_err(|e| e.to_string())
}

/// Starts the node's one rescue run after `dead_run` with `suffix` on its prompt. `Ok(true)` when
/// it was started; `Ok(false)` when the allowance is spent or the tree's budget refuses a paid run,
/// and the caller parks as it always did.
pub(super) async fn attempt(
    state: &AppState,
    node_id: &str,
    dead_run: &str,
    suffix: &str,
) -> Result<bool, String> {
    let key = format!("{RESCUE_PREFIX}{dead_run}");
    let diagnosed = spent(&state.pool, node_id, &key).await? >= 1;
    let rung = next_step(
        Kind::Rescue,
        Spent {
            diagnosed,
            ..Spent::default()
        },
    );
    if rung != Step::Diagnose {
        return Ok(false);
    }
    if budget_refusal(&state.pool, node_id).await.is_some() {
        return Ok(false);
    }
    relaunch_node(state, node_id, dead_run, &key, Some(suffix)).await?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tail_keeps_the_end_on_a_character_boundary() {
        assert_eq!(tail("abcdef", 3), "def");
        assert_eq!(tail("abc", 10), "abc");
        // 'é' is two bytes: a cut inside it moves forward.
        assert_eq!(tail("aéb", 2), "b");
    }

    #[test]
    fn a_stopped_suffix_names_the_outcome_and_carries_the_handoff() {
        let suffix = stopped_suffix("blocked", Some("  waiting on the schema \n"));
        assert!(suffix.contains("ended blocked"), "{suffix}");
        assert!(suffix.contains("waiting on the schema"), "{suffix}");
    }

    #[test]
    fn a_stopped_suffix_without_a_handoff_says_nothing_of_one() {
        for handoff in [None, Some("   ")] {
            let suffix = stopped_suffix("failed", handoff);
            assert!(!suffix.contains("last handoff"), "{suffix}");
        }
    }
}
