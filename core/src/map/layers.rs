//! §spec mapa-do-projeto
//!
//! Layers of a project's modules: derived from the import graph, frozen once, judged afterwards.
//!
//! **Pure** — paths and imports in, layers out; no SQL and no HTTP. Owner decision 2026-10-11,
//! "camadas geradas e congeladas": the layering is computed from the tree the first time the map
//! is read, kept as a baseline, and every later import that climbs a layer is a violation.

use crate::map::Import;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

/// The highest layer an owner may move a module to; the route that takes the move refuses more.
pub const MAX_LAYER: u32 = 1024;

/// The layering as it was the day it was frozen: every module's layer and every import edge.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Baseline {
    pub layers: BTreeMap<String, u32>,
    pub imports: BTreeSet<(String, String)>,
}

/// An import whose importer sits on a lower layer than the module it imports.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Violation {
    pub from: String,
    pub to: String,
    pub from_layer: u32,
    pub to_layer: u32,
    /// True when the frozen baseline did not hold this edge: the climb is recent.
    pub new: bool,
}

/// What the map shows: a layer for every module, the owner's moves, and the climbing imports.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Layering {
    pub modules: BTreeMap<String, u32>,
    pub overridden: Vec<String>,
    pub violations: Vec<Violation>,
}

/// Layers for `paths`: a module importing nothing is 0, every other is one above the highest
/// module it imports (the longest path over the condensation of the import graph). Modules in one
/// import cycle share a layer. Entries of `fixed` keep their value; the unfixed members of a
/// component sit one above the highest module outside it that the component imports.
pub fn assign(
    paths: &[String],
    imports: &[Import],
    fixed: &BTreeMap<String, u32>,
) -> BTreeMap<String, u32> {
    let names: Vec<&String> = paths.iter().collect::<BTreeSet<_>>().into_iter().collect();
    let index: BTreeMap<&str, usize> = names
        .iter()
        .enumerate()
        .map(|(i, name)| (name.as_str(), i))
        .collect();

    let mut adjacency: Vec<BTreeSet<usize>> = vec![BTreeSet::new(); names.len()];
    for import in imports {
        if let (Some(&from), Some(&to)) = (
            index.get(import.from.as_str()),
            index.get(import.to.as_str()),
        ) && from != to
        {
            adjacency[from].insert(to);
        }
    }
    let adjacency: Vec<Vec<usize>> = adjacency
        .into_iter()
        .map(|targets| targets.into_iter().collect())
        .collect();

    let mut assigned: Vec<u32> = vec![0; names.len()];
    for component in components(&adjacency) {
        let members: BTreeSet<usize> = component.iter().copied().collect();
        let mut base = 0;
        for &member in &component {
            for &target in &adjacency[member] {
                if !members.contains(&target) {
                    base = base.max(assigned[target] + 1);
                }
            }
        }
        for &member in &component {
            assigned[member] = fixed.get(names[member].as_str()).copied().unwrap_or(base);
        }
    }

    names
        .iter()
        .enumerate()
        .map(|(i, name)| ((*name).clone(), assigned[i]))
        .collect()
}

/// Strongly connected components, iterative Tarjan, sinks first: a component comes out only after
/// every component it imports. Members of a component are sorted.
fn components(adjacency: &[Vec<usize>]) -> Vec<Vec<usize>> {
    const UNVISITED: usize = usize::MAX;
    let n = adjacency.len();
    let mut order = vec![UNVISITED; n];
    let mut low = vec![0usize; n];
    let mut on_stack = vec![false; n];
    let mut stack: Vec<usize> = Vec::new();
    let mut next = 0usize;
    let mut out: Vec<Vec<usize>> = Vec::new();

    for root in 0..n {
        if order[root] != UNVISITED {
            continue;
        }
        // (node, index of the next target to look at)
        let mut work: Vec<(usize, usize)> = vec![(root, 0)];
        order[root] = next;
        low[root] = next;
        next += 1;
        stack.push(root);
        on_stack[root] = true;

        while let Some(&(node, cursor)) = work.last() {
            if cursor < adjacency[node].len() {
                if let Some(frame) = work.last_mut() {
                    frame.1 += 1;
                }
                let target = adjacency[node][cursor];
                if order[target] == UNVISITED {
                    order[target] = next;
                    low[target] = next;
                    next += 1;
                    stack.push(target);
                    on_stack[target] = true;
                    work.push((target, 0));
                } else if on_stack[target] {
                    low[node] = low[node].min(order[target]);
                }
            } else {
                work.pop();
                if let Some(&(parent, _)) = work.last() {
                    low[parent] = low[parent].min(low[node]);
                }
                if low[node] == order[node] {
                    let mut component = Vec::new();
                    while let Some(member) = stack.pop() {
                        on_stack[member] = false;
                        component.push(member);
                        if member == node {
                            break;
                        }
                    }
                    component.sort_unstable();
                    out.push(component);
                }
            }
        }
    }
    out
}

/// The layering the map shows: the frozen baseline kept, the owner's moves laid over it, modules
/// the baseline never saw placed above what they import, and every climbing import judged.
pub fn layering(
    paths: &[String],
    imports: &[Import],
    baseline: &Baseline,
    overrides: &BTreeMap<String, u32>,
) -> Layering {
    let present: BTreeSet<&str> = paths.iter().map(String::as_str).collect();

    let mut fixed: BTreeMap<String, u32> = baseline
        .layers
        .iter()
        .filter(|(path, _)| present.contains(path.as_str()))
        .map(|(path, layer)| (path.clone(), *layer))
        .collect();
    let overridden: Vec<String> = overrides
        .keys()
        .filter(|path| present.contains(path.as_str()))
        .cloned()
        .collect();
    for path in &overridden {
        fixed.insert(path.clone(), overrides[path]);
    }

    let modules = assign(paths, imports, &fixed);

    let mut violations: Vec<Violation> = imports
        .iter()
        .filter_map(|import| {
            let from_layer = *modules.get(&import.from)?;
            let to_layer = *modules.get(&import.to)?;
            (from_layer < to_layer).then(|| Violation {
                from: import.from.clone(),
                to: import.to.clone(),
                from_layer,
                to_layer,
                new: !baseline
                    .imports
                    .contains(&(import.from.clone(), import.to.clone())),
            })
        })
        .collect();
    violations.sort_by(|a, b| (&a.from, &a.to).cmp(&(&b.from, &b.to)));
    violations.dedup();

    Layering {
        modules,
        overridden,
        violations,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::map::Import;
    use std::collections::{BTreeMap, BTreeSet};

    fn imp(from: &str, to: &str) -> Import {
        Import {
            from: from.to_string(),
            to: to.to_string(),
        }
    }

    fn paths(names: &[&str]) -> Vec<String> {
        names.iter().map(|n| n.to_string()).collect()
    }

    #[test]
    fn a_module_that_imports_nothing_is_the_foundation_and_an_importer_sits_above_its_highest_import()
     {
        let p = paths(&["a.rs", "b.rs", "c.rs", "d.rs"]);
        // a -> b -> c, and d imports nothing at all.
        let imports = vec![imp("a.rs", "b.rs"), imp("b.rs", "c.rs")];

        let layers = assign(&p, &imports, &BTreeMap::new());

        assert_eq!(layers["c.rs"], 0);
        assert_eq!(layers["d.rs"], 0);
        assert_eq!(layers["b.rs"], 1);
        assert_eq!(layers["a.rs"], 2);

        // The longest path wins: a also imports c directly, and stays at 2.
        let imports = vec![
            imp("a.rs", "b.rs"),
            imp("b.rs", "c.rs"),
            imp("a.rs", "c.rs"),
        ];
        assert_eq!(assign(&p, &imports, &BTreeMap::new())["a.rs"], 2);
    }

    #[test]
    fn modules_in_one_cycle_share_a_layer() {
        let p = paths(&["a.rs", "b.rs", "c.rs", "d.rs"]);
        // a <-> b is one cycle; it imports c; d imports the cycle.
        let imports = vec![
            imp("a.rs", "b.rs"),
            imp("b.rs", "a.rs"),
            imp("b.rs", "c.rs"),
            imp("d.rs", "a.rs"),
        ];

        let layers = assign(&p, &imports, &BTreeMap::new());

        assert_eq!(layers["c.rs"], 0);
        assert_eq!(layers["a.rs"], 1);
        assert_eq!(layers["b.rs"], 1);
        assert_eq!(layers["d.rs"], 2);
    }

    #[test]
    fn a_frozen_layer_is_kept_and_a_new_module_lands_above_what_it_imports() {
        let p = paths(&["a.rs", "b.rs", "n.rs"]);
        let baseline = Baseline {
            layers: BTreeMap::from([("a.rs".to_string(), 3), ("b.rs".to_string(), 7)]),
            imports: BTreeSet::new(),
        };
        // n is new: absent from the baseline, it imports both frozen modules.
        let imports = vec![imp("n.rs", "a.rs"), imp("n.rs", "b.rs")];

        let l = layering(&p, &imports, &baseline, &BTreeMap::new());

        assert_eq!(l.modules["a.rs"], 3, "a frozen layer is never recomputed");
        assert_eq!(l.modules["b.rs"], 7);
        assert_eq!(l.modules["n.rs"], 8, "one above the highest import");
        assert!(l.overridden.is_empty());
    }

    #[test]
    fn an_import_that_climbs_a_layer_is_a_violation_and_new_only_if_the_baseline_lacked_it() {
        let p = paths(&["a.rs", "b.rs", "c.rs", "d.rs"]);
        let baseline = Baseline {
            layers: BTreeMap::from([
                ("a.rs".to_string(), 0),
                ("b.rs".to_string(), 1),
                ("c.rs".to_string(), 0),
                ("d.rs".to_string(), 1),
            ]),
            // a -> b already climbed when the baseline was frozen; c -> d did not exist.
            imports: BTreeSet::from([("a.rs".to_string(), "b.rs".to_string())]),
        };
        let imports = vec![
            imp("c.rs", "d.rs"),
            imp("a.rs", "b.rs"),
            // A downward import is no violation.
            imp("d.rs", "c.rs"),
        ];

        let l = layering(&p, &imports, &baseline, &BTreeMap::new());

        assert_eq!(
            l.violations,
            vec![
                Violation {
                    from: "a.rs".to_string(),
                    to: "b.rs".to_string(),
                    from_layer: 0,
                    to_layer: 1,
                    new: false,
                },
                Violation {
                    from: "c.rs".to_string(),
                    to: "d.rs".to_string(),
                    from_layer: 0,
                    to_layer: 1,
                    new: true,
                },
            ]
        );
    }

    #[test]
    fn an_owner_override_beats_the_baseline() {
        let p = paths(&["a.rs", "b.rs"]);
        let baseline = Baseline {
            layers: BTreeMap::from([("a.rs".to_string(), 0), ("b.rs".to_string(), 1)]),
            imports: BTreeSet::new(),
        };
        let overrides = BTreeMap::from([
            ("a.rs".to_string(), 9),
            // An override for a path that no longer exists is ignored, not reported.
            ("gone.rs".to_string(), 4),
        ]);

        let l = layering(&p, &[], &baseline, &overrides);

        assert_eq!(l.modules["a.rs"], 9);
        assert_eq!(l.modules["b.rs"], 1);
        assert_eq!(l.overridden, vec!["a.rs".to_string()]);
    }
}
