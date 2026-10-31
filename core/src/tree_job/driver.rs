//! Tree-job driver tick: advances running tree jobs one step per pass (scaffold stub).

use crate::state::AppState;

pub async fn tick(_state: &AppState) {}

/// Advance one stored tree job. GREEN owns the driver state machine.
pub async fn advance_job(_state: &AppState, _job_id: &str) {
    todo!("RED C1: implement the tree-job driver")
}

#[cfg(test)]
mod tests {
    use super::advance_job;

    macro_rules! red_driver_test {
        ($name:ident, $expectation:literal) => {
            #[tokio::test]
            async fn $name() {
                let _ = advance_job;
                panic!("RED C1: {}", $expectation);
            }
        };
    }

    red_driver_test!(
        a_green_branch_is_submitted_then_settled_into_the_feature_branch,
        "a green branch must submit and settle into the feature branch"
    );
    red_driver_test!(
        a_branch_with_a_done_report_is_gated,
        "a completed branch report must start a gate"
    );
    red_driver_test!(
        a_red_gate_relaunches_with_the_findings_twice_and_then_parks,
        "a red gate must relaunch twice with its findings before parking"
    );
    red_driver_test!(
        a_repeated_tick_queues_no_second_merge_and_writes_nothing,
        "a replayed tick must be write-free and queue no second merge"
    );
    red_driver_test!(
        a_dependent_branch_waits_for_its_base_to_merge,
        "a dependent branch must not merge before its base"
    );
    red_driver_test!(
        the_job_lands_only_when_the_root_is_done_and_every_branch_is_merged,
        "landing requires a completed root and only terminal branches"
    );
    red_driver_test!(
        a_held_job_finishes_without_moving_the_integration_branch,
        "the default hold mode must finish without landing"
    );
    red_driver_test!(
        an_integration_job_lands_into_the_integration_branch_and_finishes,
        "the explicit integration mode must land and finish"
    );
    red_driver_test!(
        a_land_refusal_is_recorded_and_the_job_stays_open,
        "a refused landing must be recorded without finishing the job"
    );
    red_driver_test!(
        a_tick_on_a_restarted_daemon_resumes_from_the_stored_request_ids,
        "stored merge and land request IDs must be settled after restart"
    );
    red_driver_test!(
        the_tick_holds_no_connection_while_calling_the_queue,
        "queue calls must not retain the job-lock connection"
    );
    red_driver_test!(
        a_tick_does_not_wait_for_a_gate_in_progress,
        "a running gate must not block a tick"
    );
    red_driver_test!(
        a_root_branch_with_no_diff_is_dropped,
        "an empty root branch must be dropped"
    );
}
