//! The branch state machine and sub-orchestrator grouping of a tree job.

#[cfg(test)]
mod tests {
    use std::ops::Range;

    use super::*;
    use crate::tree_job::model::{Branch, Manifest, BRANCH_STATES};

    const CODE: &str = "core";
    const DOCS: &str = "docs";
    const DROPPABLE: [&str; 6] = ["queued", "open", "building", "red", "fixing", "parked"];
    const PARKABLE: [&str; 8] = [
        "queued",
        "open",
        "building",
        "gating",
        "green",
        "red",
        "reviewing",
        "fixing",
    ];

    fn branch_in(layer: &str, state: &str) -> Branch {
        let mut branch = Branch::new("b1", &["core/src/a.rs".to_owned()], layer, None);
        branch.state = state.to_owned();
        branch
    }

    fn manifest_with(holders: &[(&str, &str, &str, &[&str])]) -> Manifest {
        let mut manifest = Manifest::new("t1", "feat/tree", "Ship it", "C:/Projects/nucleos", &[]);
        for (name, layer, state, paths) in holders {
            let paths = strings(paths);
            let mut branch = Branch::new(name, &paths, layer, None);
            branch.state = (*state).to_owned();
            manifest.branches.insert((*name).to_owned(), branch);
        }
        manifest
    }

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    fn items(range: Range<usize>) -> Group {
        Group::Items(range.collect())
    }

    /// Every state a transition may name: the manifest's states plus the docs-only `done`.
    fn every_state() -> Vec<&'static str> {
        let mut states = BRANCH_STATES.to_vec();
        states.push("done");
        states
    }

    fn assert_bad_grouping(items: usize, max_width: usize, max_depth: usize) {
        let result = group_items(items, max_width, max_depth);
        assert!(
            result.is_err(),
            "group_items({items}, {max_width}, {max_depth}) should be refused"
        );
        assert!(
            !matches!(result, Err(EngineError::IllegalTransition { .. })),
            "group_items({items}, {max_width}, {max_depth}) refused with a transition error"
        );
    }

    // --- transition table -------------------------------------------------------------------

    #[test]
    fn a_queued_branch_opens() {
        assert!(can_transition(CODE, "queued", "open"));
        assert!(can_transition(DOCS, "queued", "open"));
    }

    #[test]
    fn an_open_branch_starts_building() {
        assert!(can_transition(CODE, "open", "building"));
        assert!(can_transition(DOCS, "open", "building"));
    }

    #[test]
    fn only_a_docs_branch_goes_from_queued_straight_to_building() {
        assert!(can_transition(DOCS, "queued", "building"));
        assert!(!can_transition(CODE, "queued", "building"));
    }

    #[test]
    fn gate_results_lead_to_green_and_red() {
        // `tree.py` declares `gating` but never writes it: `run_gate` moves `building` and `fixing`
        // directly to the result, so both the direct path and the path through `gating` are legal.
        for from in ["gating", "building", "fixing"] {
            assert!(can_transition(CODE, from, "green"), "{from} to green");
            assert!(can_transition(CODE, from, "red"), "{from} to red");
        }
        assert!(can_transition(CODE, "building", "gating"));
        assert!(can_transition(CODE, "fixing", "gating"));
    }

    #[test]
    fn a_queued_branch_has_nothing_to_gate() {
        assert!(!can_transition(CODE, "queued", "gating"));
        assert!(!can_transition(CODE, "queued", "green"));
        assert!(!can_transition(CODE, "queued", "red"));
    }

    #[test]
    fn a_red_or_reviewed_branch_goes_to_fixing_and_back() {
        for from in ["red", "green", "reviewing"] {
            assert!(can_transition(CODE, from, "fixing"), "{from} to fixing");
        }
        // A drop or defer of the last finding changes no code, so the state from before the fix
        // returns without a new gate.
        assert!(can_transition(CODE, "fixing", "green"));
        assert!(can_transition(CODE, "fixing", "reviewing"));
    }

    #[test]
    fn a_green_branch_can_enter_review() {
        assert!(can_transition(CODE, "green", "reviewing"));
    }

    #[test]
    fn only_a_green_code_branch_merges() {
        for from in every_state() {
            assert_eq!(
                can_transition(CODE, from, "merged"),
                from == "green",
                "{from} to merged"
            );
        }
    }

    #[test]
    fn a_droppable_branch_can_be_dropped_and_no_other() {
        for from in every_state() {
            assert_eq!(
                can_transition(CODE, from, "dropped"),
                DROPPABLE.contains(&from),
                "{from} to dropped"
            );
        }
    }

    #[test]
    fn a_live_branch_parks_and_resumes() {
        for state in PARKABLE {
            assert!(can_transition(CODE, state, "parked"), "{state} to parked");
            assert!(can_transition(CODE, "parked", state), "parked to {state}");
        }
    }

    #[test]
    fn merged_and_dropped_have_no_way_out() {
        for layer in [CODE, DOCS] {
            for from in ["merged", "dropped"] {
                for to in every_state() {
                    assert!(
                        !can_transition(layer, from, to),
                        "{layer}: {from} to {to} should be refused"
                    );
                }
            }
        }
    }

    #[test]
    fn an_unknown_state_is_refused_on_either_side() {
        for layer in [CODE, DOCS] {
            assert!(!can_transition(layer, "queued", "nonsense"));
            assert!(!can_transition(layer, "nonsense", "open"));
            assert!(!can_transition(layer, "nonsense", "dropped"));
            assert!(!can_transition(layer, "nonsense", "nonsense"));
            assert!(!can_transition(layer, "", "open"));
        }
    }

    // --- apply_transition -------------------------------------------------------------------

    #[test]
    fn apply_transition_moves_a_branch_on_a_legal_step() {
        let mut branch = branch_in(CODE, "queued");

        assert_eq!(apply_transition(&mut branch, "open"), Ok(()));

        assert_eq!(branch.state, "open");
    }

    #[test]
    fn apply_transition_uses_the_layer_of_the_branch() {
        let mut docs = branch_in(DOCS, "queued");
        assert_eq!(apply_transition(&mut docs, "building"), Ok(()));
        assert_eq!(docs.state, "building");

        let mut code = branch_in(CODE, "queued");
        assert!(apply_transition(&mut code, "building").is_err());
        assert_eq!(code.state, "queued");
    }

    #[test]
    fn apply_transition_refuses_and_leaves_the_branch_unchanged() {
        let mut branch = branch_in(CODE, "merged");
        let before = branch.clone();

        let refused = apply_transition(&mut branch, "building");

        assert_eq!(
            refused,
            Err(EngineError::IllegalTransition {
                from: "merged".to_owned(),
                to: "building".to_owned(),
            })
        );
        assert_eq!(branch, before);
    }

    // --- R3: the life of a branch node ------------------------------------------------------

    #[test]
    fn a_code_branch_node_closes_on_merged_and_on_dropped_only() {
        for state in BRANCH_STATES {
            assert_eq!(
                branch_node_may_close(&branch_in(CODE, state)),
                state == "merged" || state == "dropped",
                "{state}"
            );
        }
    }

    #[test]
    fn a_child_attaches_while_a_branch_still_has_work_ahead() {
        for state in ["building", "red", "green", "reviewing", "fixing"] {
            assert!(may_attach_child(&branch_in(CODE, state)), "{state}");
        }
    }

    #[test]
    fn no_child_attaches_to_a_merged_or_dropped_branch() {
        for state in ["merged", "dropped"] {
            assert!(!may_attach_child(&branch_in(CODE, state)), "{state}");
        }
    }

    // --- R6: docs branches ------------------------------------------------------------------

    #[test]
    fn only_a_code_branch_needs_a_worktree() {
        assert!(needs_worktree(&branch_in(CODE, "queued")));
        assert!(!needs_worktree(&branch_in(DOCS, "queued")));
    }

    #[test]
    fn a_docs_branch_never_merges() {
        for from in every_state() {
            assert!(!can_transition(DOCS, from, "merged"), "{from} to merged");
        }
    }

    #[test]
    fn a_docs_branch_ends_done_or_dropped() {
        assert!(can_transition(DOCS, "building", "done"));
        assert!(can_transition(DOCS, "building", "dropped"));
        assert!(can_transition(DOCS, "queued", "dropped"));
        for to in every_state() {
            assert!(!can_transition(DOCS, "done", to), "done to {to}");
        }
    }

    #[test]
    fn a_docs_branch_node_closes_on_done_and_on_dropped() {
        assert!(branch_node_may_close(&branch_in(DOCS, "done")));
        assert!(branch_node_may_close(&branch_in(DOCS, "dropped")));
        assert!(!branch_node_may_close(&branch_in(DOCS, "queued")));
        assert!(!branch_node_may_close(&branch_in(DOCS, "building")));
    }

    #[test]
    fn done_is_refused_for_a_non_docs_layer() {
        for layer in [CODE, "shell", "sidecar"] {
            for from in every_state() {
                assert!(
                    !can_transition(layer, from, "done"),
                    "{layer}: {from} to done"
                );
            }
        }
        let mut branch = branch_in(CODE, "building");
        assert_eq!(
            apply_transition(&mut branch, "done"),
            Err(EngineError::IllegalTransition {
                from: "building".to_owned(),
                to: "done".to_owned(),
            })
        );
        assert_eq!(branch.state, "building");
    }

    // --- path overlap -----------------------------------------------------------------------

    #[test]
    fn overlapping_branch_names_a_live_holder() {
        let manifest = manifest_with(&[("core", CODE, "building", &["core/src/**"])]);

        assert_eq!(
            overlapping_branch(&manifest, &strings(&["core/src/a.rs"])),
            Some("core".to_owned())
        );
    }

    #[test]
    fn overlapping_branch_is_none_for_disjoint_paths() {
        let manifest = manifest_with(&[("core", CODE, "building", &["core/src/**"])]);

        assert_eq!(overlapping_branch(&manifest, &strings(&["shell/**"])), None);
    }

    #[test]
    fn overlapping_branch_ignores_a_merged_or_dropped_holder() {
        for state in ["merged", "dropped"] {
            let manifest = manifest_with(&[("core", CODE, state, &["core/src/**"])]);

            assert_eq!(
                overlapping_branch(&manifest, &strings(&["core/src/a.rs"])),
                None,
                "{state}"
            );
        }
    }

    #[test]
    fn overlapping_branch_looks_past_a_finished_holder_to_a_live_one() {
        let manifest = manifest_with(&[
            ("a-merged", CODE, "merged", &["core/src/**"]),
            ("z-live", CODE, "red", &["core/src/**"]),
        ]);

        assert_eq!(
            overlapping_branch(&manifest, &strings(&["core/src/a.rs"])),
            Some("z-live".to_owned())
        );
    }

    #[test]
    fn a_parked_branch_still_holds_its_paths() {
        // `branch_add` skips only `merged` and `dropped`: a parked branch comes back to its paths.
        let manifest = manifest_with(&[("core", CODE, "parked", &["core/src/**"])]);

        assert_eq!(
            overlapping_branch(&manifest, &strings(&["core/src/a.rs"])),
            Some("core".to_owned())
        );
    }

    #[test]
    fn a_docs_branch_holds_its_paths_like_any_other() {
        let manifest = manifest_with(&[("spec", DOCS, "building", &["docs/specs/**"])]);

        assert_eq!(
            overlapping_branch(&manifest, &strings(&["docs/specs/a.md"])),
            Some("spec".to_owned())
        );
    }

    // --- L11: grouping ----------------------------------------------------------------------

    #[test]
    fn eleven_items_of_width_six_split_into_two_children() {
        assert_eq!(
            group_items(11, 6, 3).unwrap(),
            Group::Children(vec![items(0..6), items(6..11)])
        );
    }

    #[test]
    fn items_within_the_width_fit_one_group() {
        assert_eq!(group_items(6, 6, 3).unwrap(), items(0..6));
        assert_eq!(group_items(1, 6, 3).unwrap(), items(0..1));
        assert_eq!(group_items(6, 6, 1).unwrap(), items(0..6));
    }

    #[test]
    fn eight_items_of_width_two_make_three_levels() {
        assert_eq!(
            group_items(8, 2, 3).unwrap(),
            Group::Children(vec![
                Group::Children(vec![items(0..2), items(2..4)]),
                Group::Children(vec![items(4..6), items(6..8)]),
            ])
        );
    }

    #[test]
    fn thirty_seven_items_of_width_six_use_as_few_children_as_each_level_allows() {
        assert_eq!(
            group_items(37, 6, 3).unwrap(),
            Group::Children(vec![
                Group::Children(vec![items(0..5), items(5..10), items(10..15), items(15..19)]),
                Group::Children(vec![items(19..25), items(25..31), items(31..37)]),
            ])
        );
    }

    #[test]
    fn items_exactly_at_capacity_fill_every_child() {
        assert_eq!(
            group_items(36, 6, 2).unwrap(),
            Group::Children(vec![
                items(0..6),
                items(6..12),
                items(12..18),
                items(18..24),
                items(24..30),
                items(30..36),
            ])
        );
    }

    #[test]
    fn items_above_capacity_are_refused() {
        assert_bad_grouping(37, 6, 2);
        assert_bad_grouping(7, 6, 1);
        assert_bad_grouping(217, 6, 3);
    }

    #[test]
    fn zero_items_are_refused() {
        assert_bad_grouping(0, 6, 3);
    }

    #[test]
    fn a_width_below_two_is_refused() {
        assert_bad_grouping(1, 1, 3);
        assert_bad_grouping(1, 0, 3);
    }

    #[test]
    fn a_depth_of_zero_is_refused() {
        assert_bad_grouping(1, 6, 0);
    }

    #[test]
    fn a_capacity_beyond_usize_does_not_panic() {
        // Python's `w ** d` is unbounded, so the port must not overflow where `build_plan` succeeds.
        assert_eq!(group_items(3, 1000, 100).unwrap(), items(0..3));
    }
}
