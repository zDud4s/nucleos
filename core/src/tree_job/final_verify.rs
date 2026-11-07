//! The final verify of a tree, at the exact tip being landed.
//!
//! The tree's landing goes through the merge queue, and with the project's `gate_before_publish`
//! the queue measures the computed merge (every group, never narrowed) before it publishes
//! anything. So the whole verify before the land already exists, at the very commit that would
//! land, and a second one here would only measure the same tree twice. What this module adds is
//! what the driver does with the queue's answer:
//!
//! - a red there is a verdict about an unchanged tip, so retrying the landing would repeat it
//!   `LAND_ATTEMPTS` times for nothing: the owner is asked at once, and the question carries the
//!   failing tests (the request's `output_tail`);
//! - an errored gate, or any other failure, stays a counted attempt as before.

use serde_json::json;
use sqlx::SqlitePool;

use super::landing::LandSettlement;
use super::rescue::tail;
use super::store::TreeJob;

/// The sentence the queue opens a red merge gate's failure with (`merge_gate_failed`).
const RED_MARKER: &str = "does not pass this project's gate";
/// Longest tail of the failing tests that goes into the question.
const FAILING_MAX: usize = 1500;

/// Whether a landing's failure reason is the merge gate's red, as opposed to an error.
pub(super) fn is_gate_red(reason: &str) -> bool {
    reason.contains(RED_MARKER)
}

/// The queue request's output, which holds the failing tests of a red gate.
async fn failing_tests(pool: &SqlitePool, request: i64) -> Option<String> {
    let output: Option<String> = sqlx::query_scalar::<_, Option<String>>(
        "SELECT output_tail FROM vcs_requests WHERE id = ?",
    )
    .bind(request)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten()
    .flatten();
    output.filter(|text| !text.trim().is_empty())
}

/// A landing the queue refused on a red gate carries the failing tests in its reason, in the job's
/// `land_failure` too; any other settlement is returned as it was.
pub(super) async fn with_failing_tests(
    pool: &SqlitePool,
    job: &mut TreeJob,
    request: i64,
    settled: LandSettlement,
) -> LandSettlement {
    let LandSettlement::NotLanded { reason } = settled else {
        return settled;
    };
    if !is_gate_red(&reason) {
        return LandSettlement::NotLanded { reason };
    }
    let reason = match failing_tests(pool, request).await {
        Some(output) => format!(
            "{reason}\nFailing output of the final verify:\n{}",
            tail(output.trim(), FAILING_MAX)
        ),
        None => reason,
    };
    job.manifest
        .extra
        .insert("land_failure".to_owned(), json!(reason));
    LandSettlement::NotLanded { reason }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_merge_gates_red_is_a_gate_red() {
        let red = "the merge does not pass this project's gate, so nothing was published.";
        assert!(is_gate_red(red));
        assert!(!is_gate_red(
            "the merge could not be measured, so nothing was published"
        ));
        assert!(!is_gate_red("hook-refused-1"));
    }
}
