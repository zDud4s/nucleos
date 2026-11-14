//! The driver closes a node that kept ending as done when its branch holds commits and a report
//! (owner decision 2026-11-13), instead of relaunching it or asking.

use super::finished_despite_ending;

fn some(result: &str) -> Option<String> {
    Some(result.to_owned())
}

#[test]
fn commits_and_a_done_report_close_the_node() {
    assert!(finished_despite_ending(3, &[some("done")]));
}

#[test]
fn a_handoff_report_without_a_result_counts_as_a_report() {
    assert!(finished_despite_ending(1, &[None]));
}

#[test]
fn no_commits_keeps_the_ladder() {
    assert!(!finished_despite_ending(0, &[some("done")]));
}

#[test]
fn no_report_keeps_the_ladder() {
    assert!(!finished_despite_ending(4, &[]));
}

#[test]
fn a_blocked_or_failed_report_is_not_done() {
    assert!(!finished_despite_ending(4, &[some("blocked")]));
    assert!(!finished_despite_ending(4, &[some("done"), some("failed")]));
}
