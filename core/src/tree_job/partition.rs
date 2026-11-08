//! The leaf-or-orchestrator rule and the fold of tiny pieces. Pure: no database, no I/O.
//!
//! A piece whose estimate fits `leaf_budget` is one leaf; a larger one becomes an orchestrator
//! that is split again. Pieces under a quarter of `leaf_budget` are not worth a run of their own
//! and are folded into a neighbour, as long as the merged piece still fits the budget.

/// A piece below this percentage of `leaf_budget` is folded into a neighbour.
pub const FLOOR_PERCENT: i64 = 25;

/// Whether a piece is built directly or split again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Leaf,
    Orchestrator,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Leaf => "leaf",
            Kind::Orchestrator => "orchestrator",
        }
    }
}

/// One piece of a split: its name and the tokens its build is estimated to take.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Piece {
    pub name: String,
    pub estimate: i64,
}

/// The size under which a piece is folded: 25% of the leaf budget, rounded down.
pub fn floor(leaf_budget: i64) -> i64 {
    leaf_budget * FLOOR_PERCENT / 100
}

/// `Leaf` when the estimate fits the budget (at or below it), `Orchestrator` above it.
pub fn classify(estimate: i64, leaf_budget: i64) -> Kind {
    if estimate <= leaf_budget {
        Kind::Leaf
    } else {
        Kind::Orchestrator
    }
}

fn merged(left: &Piece, right: &Piece) -> Piece {
    Piece {
        name: format!("{}+{}", left.name, right.name),
        estimate: left.estimate + right.estimate,
    }
}

/// Fold pieces under [`floor`] into a neighbour, greedy left to right in input order. A small
/// piece merges with the next one if the sum stays within `leaf_budget`, else with the previous
/// one under the same condition, else it stays alone. A merged piece is checked again, so a run
/// of tiny pieces collapses until it reaches the floor or the budget.
pub fn fold_small(pieces: Vec<Piece>, leaf_budget: i64) -> Vec<Piece> {
    let floor = floor(leaf_budget);
    let mut out: Vec<Piece> = Vec::with_capacity(pieces.len());
    let mut rest = pieces.into_iter().peekable();
    while let Some(mut cur) = rest.next() {
        while cur.estimate < floor {
            let fits_next = rest
                .peek()
                .is_some_and(|next| cur.estimate + next.estimate <= leaf_budget);
            if fits_next {
                let next = rest.next().expect("peeked");
                cur = merged(&cur, &next);
            } else {
                break;
            }
        }
        if cur.estimate < floor
            && let Some(prev) = out.last_mut()
            && prev.estimate + cur.estimate <= leaf_budget
        {
            *prev = merged(prev, &cur);
            continue;
        }
        out.push(cur);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pieces(items: &[(&str, i64)]) -> Vec<Piece> {
        items
            .iter()
            .map(|(name, estimate)| Piece {
                name: (*name).to_owned(),
                estimate: *estimate,
            })
            .collect()
    }

    fn names(items: &[Piece]) -> Vec<&str> {
        items.iter().map(|piece| piece.name.as_str()).collect()
    }

    #[test]
    fn classify_fits_at_the_budget_and_descends_above_it() {
        assert_eq!(classify(120_000, 120_000), Kind::Leaf);
        assert_eq!(classify(120_001, 120_000), Kind::Orchestrator);
    }

    #[test]
    fn kind_names_are_the_wire_values() {
        assert_eq!(Kind::Leaf.as_str(), "leaf");
        assert_eq!(Kind::Orchestrator.as_str(), "orchestrator");
    }

    #[test]
    fn floor_is_a_quarter_of_the_leaf_budget() {
        assert_eq!(floor(120_000), 30_000);
    }

    #[test]
    fn fold_small_merges_pieces_under_the_floor_into_a_neighbour() {
        // 10k + 20k are under 30k: they fold together (30k, no longer under the floor); 90k stays.
        let out = fold_small(pieces(&[("a", 10_000), ("b", 20_000), ("c", 90_000)]), 120_000);
        assert_eq!(names(&out), ["a+b", "c"]);
        assert_eq!(out[0].estimate, 30_000);
    }

    #[test]
    fn fold_small_folds_a_trailing_small_piece_into_the_previous_one() {
        let out = fold_small(pieces(&[("a", 90_000), ("b", 10_000)]), 120_000);
        assert_eq!(names(&out), ["a+b"]);
        assert_eq!(out[0].estimate, 100_000);
    }

    #[test]
    fn fold_small_never_builds_a_piece_over_the_budget() {
        // 20k + 110k would be 130k > 120k: the small piece stays alone, the floor rule yields.
        let out = fold_small(pieces(&[("a", 20_000), ("b", 110_000)]), 120_000);
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn fold_small_leaves_one_piece_and_an_empty_list_alone() {
        assert!(fold_small(Vec::new(), 120_000).is_empty());
        let one = pieces(&[("a", 5)]);
        assert_eq!(fold_small(one.clone(), 120_000), one);
    }
}
