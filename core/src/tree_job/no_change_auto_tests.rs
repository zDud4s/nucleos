//! The driver drops a branch whose work is already on its merge target (merged, superseded, or
//! commits that add nothing) instead of parking it on a question.

use super::present_on_base;

#[test]
fn a_branch_merged_into_the_target_is_dropped() {
    let reason = present_on_base(0, true, "", true, "feat/x").unwrap();
    assert!(reason.contains("already merged into feat/x"));
}

#[test]
fn a_branch_whose_every_patch_is_on_the_target_is_superseded() {
    let reason = present_on_base(2, false, "- aaa\n- bbb\n", false, "feat/x").unwrap();
    assert!(reason.starts_with("superseded"));
}

#[test]
fn one_patch_the_target_lacks_keeps_the_branch() {
    assert_eq!(
        present_on_base(2, false, "- aaa\n+ bbb\n", false, "t"),
        None
    );
}

#[test]
fn commits_with_an_empty_net_diff_add_nothing() {
    let reason = present_on_base(2, false, "+ aaa\n+ bbb\n", true, "t").unwrap();
    assert!(reason.contains("add nothing"));
}

#[test]
fn a_branch_with_no_commits_is_not_dropped() {
    // Nothing was produced and nothing is on the base: today's park stands.
    assert_eq!(present_on_base(0, false, "", true, "t"), None);
}
