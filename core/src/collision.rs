//! Two live worktrees of one project touching the same files.
//!
//! It lives apart from `concurrency.rs` because it answers a different question. That one says *how
//! much work fits*, and the answer is an invariant held by a primary key; this one says *what that
//! work is touching*, and the answer is a best-effort warning with three states. Putting them
//! together would give a reader the impression that collision is as hard as a slot, and it is not.
//!
//! **Two sources, never merged.** The declared one (`job_items.files`) is intent and arrives in
//! time for you to stop; the observed one is fact and arrives after execution has started. A
//! warning that only comes once both worktrees have written to the same file comes late — hence
//! both — and an intention presented as fact would be a lie — hence separate.

// TEMPORARY, and it goes away with the read path: until `http.rs` calls `for_project`, nothing
// reachable from `main` names anything in this module, and `cargo clippy --all-targets -- -D
// warnings` fails the gate on seven `dead_code` findings. At module level rather than seven
// attributes because it is meant to be deleted in one edit, not maintained.
#![allow(dead_code)]

use std::collections::BTreeSet;

/// The pair `worktrees` and `project_slots` use to name an owner.
///
/// `kind` is a `String` and not an `Owner`: this leaves over JSON to an interface that only wants
/// to know which tab to link to, and a serialised `enum` would force the other side to know the
/// internal shape.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
pub struct OwnerRef {
    pub kind: String,
    pub id: i64,
}

/// A coincidence between two trees, and the paths where it happens.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Overlap {
    pub a: OwnerRef,
    pub b: OwnerRef,
    pub paths: Vec<String>,
}

/// The three states, and the third exists so the second is never said in vain.
///
/// It follows `gate.rs`, which distinguishes *failed* from *never measured*. Saying `clean` without
/// having measured is the one way this screen can do active damage: somebody lets two jobs run,
/// trusting a warning nobody ever computed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Collide,
    Clean,
    NotMeasured,
}

/// One source, with a state of its own. The two answer independently and are **not** collapsed into
/// one: a project whose slots are all runs has the declared source in `not_measured` and the
/// observed one with a real answer.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Source {
    pub state: State,
    pub overlaps: Vec<Overlap>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Collisions {
    pub declared: Source,
    pub observed: Source,
}

/// PURE: every pair of trees sharing at least one path.
///
/// Stable order — the sets arrive ordered by owner and the paths by name — because this feeds a
/// screen that repaints every 3 seconds, and an order that dances makes warnings jump around with
/// nothing having changed.
///
/// Quadratic in the number of trees, and that is acceptable because the number has a ceiling: the
/// per-project one is 2 by default. Measure before raising it much.
pub fn overlaps(sets: &[(OwnerRef, BTreeSet<String>)]) -> Vec<Overlap> {
    let mut found = Vec::new();
    for (index, (a, left)) in sets.iter().enumerate() {
        for (b, right) in sets.iter().skip(index + 1) {
            let paths: Vec<String> = left.intersection(right).cloned().collect();
            if !paths.is_empty() {
                found.push(Overlap {
                    a: a.clone(),
                    b: b.clone(),
                    paths,
                });
            }
        }
    }
    found
}

/// PURE: the predicted warning, minus what the observed one already says.
///
/// When both sources name the same path it is **one** event, not two: a `Running` item declares
/// what it is going to write and has already written part of it, and the stronger source wins. What
/// is left to the predicted one are the paths not yet touched, which is the only thing it knows how
/// to say better.
///
/// The subtraction is **per pair**. A path shared between 1 and 2 says nothing about what 1 and 3
/// are going to do, and subtracting per path would erase warnings nobody confirmed.
pub fn only_predicted(declared: Vec<Overlap>, observed: &[Overlap]) -> Vec<Overlap> {
    declared
        .into_iter()
        .filter_map(|mut overlap| {
            if let Some(confirmed) = observed
                .iter()
                .find(|other| other.a == overlap.a && other.b == overlap.b)
            {
                overlap.paths.retain(|path| !confirmed.paths.contains(path));
            }
            (!overlap.paths.is_empty()).then_some(overlap)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn owner(kind: &str, id: i64) -> OwnerRef {
        OwnerRef {
            kind: kind.to_string(),
            id,
        }
    }

    fn set(paths: &[&str]) -> std::collections::BTreeSet<String> {
        paths.iter().map(|path| path.to_string()).collect()
    }

    /// The ordinary case: two trees in one file.
    #[test]
    fn two_trees_touching_one_file_are_one_overlap_naming_both() {
        let sets = vec![
            (
                owner("job", 1),
                set(&["core/src/runs.rs", "core/src/job.rs"]),
            ),
            (
                owner("run", 7),
                set(&["core/src/runs.rs", "shell/src/App.tsx"]),
            ),
        ];

        let found = overlaps(&sets);

        assert_eq!(found.len(), 1);
        assert_eq!(found[0].a, owner("job", 1));
        assert_eq!(found[0].b, owner("run", 7));
        assert_eq!(found[0].paths, vec!["core/src/runs.rs".to_string()]);
    }

    /// A lone tree collides with nobody, and two disjoint trees do not either.
    #[test]
    fn disjoint_sets_and_a_lone_set_produce_nothing() {
        assert!(overlaps(&[(owner("job", 1), set(&["a.rs"]))]).is_empty());
        assert!(
            overlaps(&[
                (owner("job", 1), set(&["a.rs"])),
                (owner("job", 2), set(&["b.rs"])),
            ])
            .is_empty()
        );
    }

    /// Three trees give three pairs, and each pair is named exactly once.
    #[test]
    fn three_trees_on_one_file_give_three_pairs_each_named_once() {
        let sets = vec![
            (owner("job", 1), set(&["a.rs"])),
            (owner("job", 2), set(&["a.rs"])),
            (owner("run", 3), set(&["a.rs"])),
        ];

        let found = overlaps(&sets);

        assert_eq!(found.len(), 3);
        let pairs: Vec<(i64, i64)> = found
            .iter()
            .map(|overlap| (overlap.a.id, overlap.b.id))
            .collect();
        assert_eq!(pairs, vec![(1, 2), (1, 3), (2, 3)]);
    }

    /// When both sources name the same path it is one event, not two: the stronger one wins. What
    /// is left to the predicted source is what has not been touched yet, which is the only thing it
    /// knows how to say better.
    #[test]
    fn a_path_both_sources_name_is_left_to_the_observed_one() {
        let declared = vec![Overlap {
            a: owner("job", 1),
            b: owner("job", 2),
            paths: vec!["written.rs".into(), "still_to_write.rs".into()],
        }];
        let observed = vec![Overlap {
            a: owner("job", 1),
            b: owner("job", 2),
            paths: vec!["written.rs".into()],
        }];

        let predicted = only_predicted(declared, &observed);

        assert_eq!(predicted.len(), 1);
        assert_eq!(predicted[0].paths, vec!["still_to_write.rs".to_string()]);
    }

    /// A pair whose prediction was entirely confirmed leaves the predicted source — otherwise the
    /// card would show two warnings for a single event.
    #[test]
    fn a_pair_the_observed_source_fully_covers_leaves_the_predicted_one() {
        let declared = vec![Overlap {
            a: owner("job", 1),
            b: owner("job", 2),
            paths: vec!["written.rs".into()],
        }];
        let observed = declared.clone();

        assert!(only_predicted(declared, &observed).is_empty());
    }

    /// Different pairs do not subtract from each other. A path shared between 1 and 2 says nothing
    /// about what 1 and 3 are going to do.
    #[test]
    fn the_subtraction_is_per_pair_and_not_per_path() {
        let declared = vec![Overlap {
            a: owner("job", 1),
            b: owner("job", 3),
            paths: vec!["shared.rs".into()],
        }];
        let observed = vec![Overlap {
            a: owner("job", 1),
            b: owner("job", 2),
            paths: vec!["shared.rs".into()],
        }];

        assert_eq!(only_predicted(declared.clone(), &observed), declared);
    }
}
