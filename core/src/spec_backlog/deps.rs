//! Pure dependency planning for specs: a topological order, a cycle reported as a question for the
//! owner instead of being cut silently, and the groups of specs that may run in parallel.

use std::collections::{BTreeMap, BTreeSet};

use crate::work_nodes::sets_overlap;

/// Specs that depend on each other in a loop. Downstream specs that merely wait on the loop are
/// not members.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cycle {
    pub specs: Vec<String>,
}

impl Cycle {
    /// The question put to the owner instead of cutting the loop silently.
    pub fn owner_question(&self) -> String {
        format!(
            "These specs depend on each other in a loop: {}. Which dependency should be dropped \
             or reversed so they can be ordered?",
            self.specs.join(", ")
        )
    }
}

/// A spec to schedule, with the paths it will write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedSpec {
    pub id: String,
    /// Empty means the scope is unknown, so the spec runs alone.
    pub owned_paths: Vec<String>,
}

/// In-set dependencies as `spec -> specs it waits on`; edges touching an outside id are ignored.
fn in_set_deps(
    specs: &BTreeSet<String>,
    deps: &[(String, String)],
) -> BTreeMap<String, BTreeSet<String>> {
    let mut map: BTreeMap<String, BTreeSet<String>> =
        specs.iter().map(|id| (id.clone(), BTreeSet::new())).collect();
    for (spec, depends_on) in deps {
        if specs.contains(spec) && specs.contains(depends_on) {
            map.entry(spec.clone()).or_default().insert(depends_on.clone());
        }
    }
    map
}

/// Everything reachable from `start` by following dependencies (not including `start` unless a
/// loop leads back to it).
fn reachable(start: &str, map: &BTreeMap<String, BTreeSet<String>>) -> BTreeSet<String> {
    let mut seen = BTreeSet::new();
    let mut stack: Vec<&String> = map.get(start).into_iter().flatten().collect();
    while let Some(next) = stack.pop() {
        if seen.insert(next.clone()) {
            stack.extend(map.get(next).into_iter().flatten());
        }
    }
    seen
}

/// Dependencies first; ties broken by id, so the result is deterministic. A loop is returned as a
/// `Cycle` (its smallest member's strongly connected group), never cut.
pub fn topological_order(
    specs: &[String],
    deps: &[(String, String)],
) -> Result<Vec<String>, Cycle> {
    let ids: BTreeSet<String> = specs.iter().cloned().collect();
    let map = in_set_deps(&ids, deps);
    let mut placed: BTreeSet<&String> = BTreeSet::new();
    let mut order = Vec::with_capacity(ids.len());
    while order.len() < ids.len() {
        let ready: Vec<&String> = map
            .iter()
            .filter(|(id, waits)| !placed.contains(id) && waits.iter().all(|w| placed.contains(w)))
            .map(|(id, _)| id)
            .collect();
        if ready.is_empty() {
            // What is left holds loops and their downstream; keep only a spec that reaches itself.
            let left: Vec<&String> = map.keys().filter(|id| !placed.contains(id)).collect();
            let reach: BTreeMap<&String, BTreeSet<String>> =
                left.iter().map(|id| (*id, reachable(id, &map))).collect();
            let first = left
                .iter()
                .find(|id| reach[**id].contains(id.as_str()))
                .expect("a stuck remainder contains a loop");
            let members = left
                .iter()
                .filter(|id| {
                    reach[**id].contains(first.as_str()) && reach[*first].contains(id.as_str())
                })
                .map(|id| (*id).clone())
                .collect();
            return Err(Cycle { specs: members });
        }
        for id in ready {
            placed.insert(id);
            order.push(id.clone());
        }
    }
    Ok(order)
}

/// Groups of specs that may run together: a dependent lands in a later group than what it waits
/// on, specs with overlapping owned paths never share a group, and an unknown scope is alone.
pub fn parallel_groups(
    specs: &[PlannedSpec],
    deps: &[(String, String)],
) -> Result<Vec<Vec<String>>, Cycle> {
    let ids: Vec<String> = specs.iter().map(|spec| spec.id.clone()).collect();
    let order = topological_order(&ids, deps)?;
    let set: BTreeSet<String> = ids.iter().cloned().collect();
    let map = in_set_deps(&set, deps);

    // Level = longest chain of dependencies below the spec; `order` guarantees they are set.
    let mut level: BTreeMap<&String, usize> = BTreeMap::new();
    for id in &order {
        let below = map[id].iter().map(|dep| level[dep] + 1).max().unwrap_or(0);
        level.insert(id, below);
    }

    let mut groups: Vec<Vec<String>> = Vec::new();
    let top = level.values().copied().max().map_or(0, |max| max + 1);
    for current in 0..top {
        // Input order inside a level keeps the packing deterministic.
        let mut level_groups: Vec<Vec<&PlannedSpec>> = Vec::new();
        let mut seen = BTreeSet::new();
        for spec in specs
            .iter()
            .filter(|spec| level[&spec.id] == current && seen.insert(&spec.id))
        {
            let slot = if spec.owned_paths.is_empty() {
                None
            } else {
                level_groups.iter().position(|group| {
                    group.iter().all(|other| {
                        !other.owned_paths.is_empty()
                            && sets_overlap(&spec.owned_paths, &other.owned_paths).is_none()
                    })
                })
            };
            match slot {
                Some(index) => level_groups[index].push(spec),
                None => level_groups.push(vec![spec]),
            }
        }
        groups.extend(
            level_groups
                .into_iter()
                .map(|group| group.into_iter().map(|spec| spec.id.clone()).collect()),
        );
    }
    Ok(groups)
}

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
