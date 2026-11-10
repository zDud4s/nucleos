//! The final verify of a tree, at the exact tip being landed.
//!
//! The tree's landing goes through the merge queue, and with the project's `gate_before_publish`
//! the queue measures the computed merge (every group, never narrowed) before it publishes
//! anything. So the whole verify before the land already exists, at the very commit that would
//! land, and a second one here would only measure the same tree twice. What this module adds is
//! what the driver does with the queue's answer:
//!
//! - a red there is a verdict about an unchanged tip, so retrying the landing would repeat it:
//!   instead of asking the owner at once (autonomy-trees spec B3), the root node is started again
//!   with the failing tests as a "fix the integrated tip" run, and when it ends the landing runs
//!   the final verify again. The runs are counted on the ladder (`Kind::FinalFix`: its retries,
//!   then one rescue run, then the park), and only the park asks the owner, with the failing tests;
//! - an errored gate, or any other failure, stays a counted attempt as before.
//!
//! The state lives in `manifest.extra.final_fix`: `{state, retries, diagnosed, failing}`, with the
//! state `requested` (a red was seen, no run started), `running` (the fix run is on), `settled`
//! (it ended and the landing goes again) or `parked` (the owner was asked). An owner's answer drops
//! the record, which is a fresh allowance.

use serde_json::{Map, Value, json};
use sqlx::SqlitePool;

use super::driver::{Park, persist};
use super::ladder::{Kind, Spent, Step, next_step};
use super::landing::LandSettlement;
use super::open::run_is_live;
use super::relaunch::{FINAL_FIX_PREFIX, budget_refusal, refusal_question, relaunch_node};
use super::rescue::{self, tail};
use super::retry::{LAND_ATTEMPTS, land_question, set_land_record};
use super::store::TreeJob;
use crate::state::AppState;
use crate::work_nodes;

/// The key of `Manifest.extra` that holds the fix record of the integrated tip.
pub(super) const FINAL_FIX_KEY: &str = "final_fix";
const REQUESTED: &str = "requested";
const RUNNING: &str = "running";
const SETTLED: &str = "settled";
const PARKED: &str = "parked";
/// Longest tail of the failing output that goes into the fix run's prompt.
const PROMPT_FAILING_MAX: usize = 3000;

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

// ---- the fix of the integrated tip -------------------------------------------------------------

fn record_of(job: &TreeJob) -> Map<String, Value> {
    job.manifest
        .extra
        .get(FINAL_FIX_KEY)
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default()
}

fn store_record(job: &mut TreeJob, record: Map<String, Value>) {
    job.manifest
        .extra
        .insert(FINAL_FIX_KEY.to_owned(), Value::Object(record));
}

fn set_state(job: &mut TreeJob, state: &str) {
    let mut record = record_of(job);
    record.insert("state".to_owned(), json!(state));
    store_record(job, record);
}

/// A landing the queue refused on a red final verify asks for a fix run of the integrated tip: the
/// failing output is kept, the counts of the runs already made stay. The caller persists.
pub(super) fn request(job: &mut TreeJob, reason: &str) {
    let mut record = record_of(job);
    record.insert("state".to_owned(), json!(REQUESTED));
    record.insert(
        "failing".to_owned(),
        json!(tail(reason, PROMPT_FAILING_MAX)),
    );
    store_record(job, record);
}

/// The prompt suffix of a fix run of the integrated tip.
fn fix_suffix(failing: &str) -> String {
    format!(
        "\n\nThe final verify of the integrated tip of this tree is red: every branch passed its own gate, but the merged whole does not pass the project's gate, so nothing was published. Failing output:\n{failing}\n\nFix the integrated tip: bring your worktree to the tip the tree would land (the feature branch with every merged branch on it), fix what fails there, commit, and finish the node again. The landing then runs the final verify again."
    )
}

/// Whether the root node's run is on.
async fn root_run_live(pool: &SqlitePool, root: &str) -> Result<bool, String> {
    let node = work_nodes::get(pool, root)
        .await
        .map_err(|e| e.to_string())?;
    match node.run_id {
        Some(run) => run_is_live(pool, &run).await.map_err(|e| e.to_string()),
        None => Ok(false),
    }
}

/// Asks the owner. The allowance of the landing is spent too, so `land_allowed` holds the landing
/// until an answer, which also drops this record.
async fn park(
    pool: &SqlitePool,
    job: &mut TreeJob,
    question: String,
    reason: &str,
    parks: &mut Vec<Park>,
) -> Result<(), String> {
    set_land_record(job, LAND_ATTEMPTS, reason);
    set_state(job, PARKED);
    persist(pool, job).await?;
    parks.push((job.root_node_id.clone(), question));
    Ok(())
}

/// Advances the fix of the integrated tip one step. `Ok(true)` means the landing waits this tick
/// (a fix run is on, or the owner was just asked); `Ok(false)` that the landing may go on.
pub(super) async fn step(
    state: &AppState,
    job: &mut TreeJob,
    parks: &mut Vec<Park>,
) -> Result<bool, String> {
    let pool = &state.pool;
    let record = record_of(job);
    let root = job.root_node_id.clone();
    match record.get("state").and_then(Value::as_str) {
        Some(RUNNING) => {
            if root_run_live(pool, &root).await? {
                return Ok(true);
            }
            set_state(job, SETTLED);
            persist(pool, job).await?;
            Ok(false)
        }
        Some(REQUESTED) => {
            let retries = record.get("retries").and_then(Value::as_i64).unwrap_or(0);
            let diagnosed = record
                .get("diagnosed")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let failing = record
                .get("failing")
                .and_then(Value::as_str)
                .unwrap_or("the final verify failed without output")
                .to_owned();
            if let Some(reason) = budget_refusal(pool, &root).await {
                park(pool, job, refusal_question(&root, &reason), &failing, parks).await?;
                return Ok(true);
            }
            let dead_run = work_nodes::get(pool, &root)
                .await
                .map_err(|e| e.to_string())?
                .run_id
                .unwrap_or_default();
            let started = match next_step(
                Kind::FinalFix,
                Spent {
                    retries,
                    diagnosed,
                    ..Spent::default()
                },
            ) {
                Step::Retry { .. } => {
                    let key = format!("{FINAL_FIX_PREFIX}{dead_run}");
                    let suffix = fix_suffix(&failing);
                    match relaunch_node(state, &root, &dead_run, &key, Some(&suffix)).await {
                        Ok(_) => {
                            let mut held = record_of(job);
                            held.insert("retries".to_owned(), json!(retries + 1));
                            store_record(job, held);
                            true
                        }
                        Err(error) => {
                            tracing::warn!(job = %job.id, %error, "could not start the fix of the integrated tip");
                            false
                        }
                    }
                }
                Step::Diagnose => {
                    let suffix = rescue::red_suffix(&fix_suffix(&failing));
                    let started = rescue::attempt(state, &root, &dead_run, &suffix).await?;
                    if started {
                        let mut held = record_of(job);
                        held.insert("diagnosed".to_owned(), json!(true));
                        store_record(job, held);
                    }
                    started
                }
                Step::Park => false,
            };
            if started {
                set_state(job, RUNNING);
                persist(pool, job).await?;
                return Ok(true);
            }
            let made = format!(
                "{retries} automatic fix run(s){} of the integrated tip",
                if diagnosed { " and a rescue run" } else { "" }
            );
            let question = format!(
                "{}\n(The tree already made {made}.)",
                land_question(LAND_ATTEMPTS, &failing)
            );
            park(pool, job, question, &failing, parks).await?;
            Ok(true)
        }
        _ => Ok(false),
    }
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

    #[test]
    fn a_fix_suffix_carries_the_failing_tests_and_the_way_on() {
        let suffix = fix_suffix("FAIL core: tests::boom");
        assert!(suffix.contains("tests::boom"), "{suffix}");
        assert!(suffix.contains("integrated tip"), "{suffix}");
        assert!(suffix.contains("finish the node again"), "{suffix}");
    }
}
