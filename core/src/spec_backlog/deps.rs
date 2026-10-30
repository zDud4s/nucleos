//! Pure dependency planning for specs: a topological order, a cycle reported as a question for the
//! owner instead of being cut silently, and the groups of specs that may run in parallel.

#[cfg(test)]
mod tests {
    // `topological_order` stays qualified as `super::` here: a test of the same name, fixed by the
    // plan, shadows it in this module.
    use super::{parallel_groups, Cycle, PlannedSpec};
    use crate::work_nodes::sets_overlap;

    fn ids(specs: &[&str]) -> Vec<String> {
        specs.iter().map(|id| (*id).to_owned()).collect()
    }

    fn pairs(deps: &[(&str, &str)]) -> Vec<(String, String)> {
        deps.iter()
            .map(|(spec, depends_on)| ((*spec).to_owned(), (*depends_on).to_owned()))
            .collect()
    }

    fn planned(id: &str, owned_paths: &[&str]) -> PlannedSpec {
        PlannedSpec {
            id: id.to_owned(),
            owned_paths: ids(owned_paths),
        }
    }

    fn position(order: &[String], id: &str) -> usize {
        order
            .iter()
            .position(|candidate| candidate == id)
            .unwrap_or_else(|| panic!("{id} is missing from {order:?}"))
    }

    fn group_of(groups: &[Vec<String>], id: &str) -> usize {
        groups
            .iter()
            .position(|group| group.iter().any(|candidate| candidate == id))
            .unwrap_or_else(|| panic!("{id} is missing from {groups:?}"))
    }

    /// Every spec appears exactly once across the groups, and no group is empty.
    fn assert_each_spec_once(groups: &[Vec<String>], specs: &[PlannedSpec]) {
        let mut seen: Vec<&String> = groups.iter().flatten().collect();
        seen.sort();
        let mut expected: Vec<&String> = specs.iter().map(|spec| &spec.id).collect();
        expected.sort();
        assert_eq!(seen, expected);
        assert!(groups.iter().all(|group| !group.is_empty()), "{groups:?}");
    }

    /// The cycle with its members sorted: the rule fixes who is in it, not where it starts.
    fn sorted(mut cycle: Cycle) -> Cycle {
        cycle.specs.sort();
        cycle
    }

    fn cycle_of(specs: &[&str]) -> Cycle {
        Cycle { specs: ids(specs) }
    }

    #[test]
    fn topological_order() {
        // Chain, listed backwards so that keeping the input order would fail.
        let chain = super::topological_order(
            &ids(&["spec-c", "spec-b", "spec-a"]),
            &pairs(&[("spec-c", "spec-b"), ("spec-b", "spec-a")]),
        )
        .unwrap();
        assert_eq!(chain, ids(&["spec-a", "spec-b", "spec-c"]));

        // Diamond, also listed backwards.
        let specs = ids(&["spec-d", "spec-c", "spec-b", "spec-a"]);
        let deps = pairs(&[
            ("spec-d", "spec-b"),
            ("spec-d", "spec-c"),
            ("spec-b", "spec-a"),
            ("spec-c", "spec-a"),
        ]);
        let diamond = super::topological_order(&specs, &deps).unwrap();
        assert_eq!(diamond.len(), 4, "{diamond:?}");
        for (spec, depends_on) in &deps {
            assert!(
                position(&diamond, depends_on) < position(&diamond, spec),
                "{depends_on} must precede {spec} in {diamond:?}"
            );
        }

        // Deterministic: the two free specs in the middle do not change places between calls.
        assert_eq!(super::topological_order(&specs, &deps).unwrap(), diamond);
    }

    #[test]
    fn cycle_returns_owner_question() {
        let specs = ids(&[
            "spec-loop-a",
            "spec-loop-b",
            "spec-loop-c",
            "spec-upstream",
            "spec-downstream",
            "spec-unrelated",
        ]);
        let deps = pairs(&[
            ("spec-loop-a", "spec-loop-b"),
            ("spec-loop-b", "spec-loop-c"),
            ("spec-loop-c", "spec-loop-a"),
            // Neighbours of the cycle, which are not part of it.
            ("spec-loop-a", "spec-upstream"),
            ("spec-downstream", "spec-loop-a"),
        ]);

        let cycle = sorted(super::topological_order(&specs, &deps).unwrap_err());
        assert_eq!(
            cycle,
            cycle_of(&["spec-loop-a", "spec-loop-b", "spec-loop-c"])
        );

        let question = cycle.owner_question();
        for member in &cycle.specs {
            assert!(question.contains(member), "{member} not in {question:?}");
        }
        for outsider in ["spec-upstream", "spec-downstream", "spec-unrelated"] {
            assert!(!question.contains(outsider), "{outsider} in {question:?}");
        }

        // A spec depending on itself is a cycle of one.
        let self_cycle = super::topological_order(
            &ids(&["spec-self", "spec-unrelated"]),
            &pairs(&[("spec-self", "spec-self")]),
        )
        .unwrap_err();
        assert_eq!(self_cycle, cycle_of(&["spec-self"]));
        assert!(self_cycle.owner_question().contains("spec-self"));
    }

    #[test]
    fn independent_specs_group_for_parallel_run() {
        let specs = [
            planned("spec-a", &["core/src/alpha/**"]),
            planned("spec-b", &["core/src/beta/**"]),
            planned("spec-c", &["shell/src/gamma/**"]),
        ];

        let groups = parallel_groups(&specs, &[]).unwrap();

        assert_eq!(groups.len(), 1, "{groups:?}");
        assert_each_spec_once(&groups, &specs);
    }

    #[test]
    fn overlapping_owned_paths_are_serialised() {
        let specs = [
            planned("spec-a", &["core/src/shared/**"]),
            planned("spec-b", &["core/src/other/**", "core/src/shared/mod.rs"]),
            planned("spec-c", &["docs/guide/**"]),
        ];
        // The scenario itself: only the first two really overlap.
        assert!(sets_overlap(&specs[0].owned_paths, &specs[1].owned_paths).is_some());
        assert!(sets_overlap(&specs[0].owned_paths, &specs[2].owned_paths).is_none());
        assert!(sets_overlap(&specs[1].owned_paths, &specs[2].owned_paths).is_none());

        let groups = parallel_groups(&specs, &[]).unwrap();

        assert_each_spec_once(&groups, &specs);
        assert_ne!(
            group_of(&groups, "spec-a"),
            group_of(&groups, "spec-b"),
            "{groups:?}"
        );
    }

    #[test]
    fn dependency_outside_the_set_does_not_block() {
        let deps = pairs(&[("spec-a", "spec-elsewhere")]);

        let order = super::topological_order(&ids(&["spec-a", "spec-b"]), &deps).unwrap();
        let mut members = order.clone();
        members.sort();
        assert_eq!(members, ids(&["spec-a", "spec-b"]));

        let specs = [
            planned("spec-a", &["core/src/alpha/**"]),
            planned("spec-b", &["core/src/beta/**"]),
        ];
        let groups = parallel_groups(&specs, &deps).unwrap();
        assert_eq!(groups.len(), 1, "{groups:?}");
        assert_each_spec_once(&groups, &specs);
    }

    #[test]
    fn dependent_runs_in_a_later_group() {
        // Listed dependent first, and with disjoint paths: only the dependency separates them.
        let specs = [
            planned("spec-b", &["core/src/beta/**"]),
            planned("spec-a", &["core/src/alpha/**"]),
        ];

        let groups = parallel_groups(&specs, &pairs(&[("spec-b", "spec-a")])).unwrap();

        assert_eq!(groups, vec![ids(&["spec-a"]), ids(&["spec-b"])]);
    }

    #[test]
    fn unknown_scope_runs_alone() {
        let specs = [
            planned("spec-known-a", &["core/src/alpha/**"]),
            planned("spec-unknown-1", &[]),
            planned("spec-known-b", &["core/src/beta/**"]),
            planned("spec-unknown-2", &[]),
        ];

        let groups = parallel_groups(&specs, &[]).unwrap();

        assert_each_spec_once(&groups, &specs);
        for unknown in ["spec-unknown-1", "spec-unknown-2"] {
            assert_eq!(
                groups[group_of(&groups, unknown)],
                ids(&[unknown]),
                "{groups:?}"
            );
        }
    }

    #[test]
    fn parallel_groups_reports_a_cycle() {
        let specs = [
            planned("spec-a", &["core/src/alpha/**"]),
            planned("spec-b", &["core/src/beta/**"]),
            planned("spec-c", &["core/src/gamma/**"]),
        ];
        let deps = pairs(&[("spec-a", "spec-b"), ("spec-b", "spec-a")]);

        let cycle = sorted(parallel_groups(&specs, &deps).unwrap_err());

        assert_eq!(cycle, cycle_of(&["spec-a", "spec-b"]));
    }
}
