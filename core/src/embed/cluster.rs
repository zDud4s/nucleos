//! One deterministic grouping shared by the generalisation pass (0.80) and the themes (0.70), spec B12.
use super::cosine;

pub struct Params {
    pub threshold: f32,
    pub min_size: usize,
    pub max_size: usize,
}

/// Seed-anchored groups: ids ascending; each unclaimed id seeds a group with the unclaimed ids whose cosine
/// to the SEED is >= `threshold` (no chaining), closest first, cut to `max_size`. Members are sorted by id
/// and groups come out ordered by their smallest id.
pub fn cluster(items: &[(i64, &[f32])], params: Params) -> Vec<Vec<i64>> {
    let mut order: Vec<usize> = (0..items.len()).collect();
    order.sort_by_key(|&i| items[i].0);
    let mut taken = vec![false; items.len()];
    let mut groups = Vec::new();
    for &seed in &order {
        if taken[seed] {
            continue;
        }
        taken[seed] = true;
        let mut near: Vec<(f32, i64, usize)> = order
            .iter()
            .filter(|&&o| !taken[o])
            .filter_map(|&o| {
                cosine(items[seed].1, items[o].1)
                    .filter(|s| *s >= params.threshold)
                    .map(|s| (s, items[o].0, o))
            })
            .collect();
        near.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
        near.truncate(params.max_size.saturating_sub(1));
        let mut ids = vec![items[seed].0];
        for (_, id, index) in near {
            taken[index] = true;
            ids.push(id);
        }
        ids.sort_unstable();
        if ids.len() >= params.min_size {
            groups.push(ids);
        }
    }
    groups
}

#[cfg(test)]
mod tests {
    use super::{Params, cluster};
    fn p(threshold: f32, min_size: usize, max_size: usize) -> Params {
        Params {
            threshold,
            min_size,
            max_size,
        }
    }
    const X: [f32; 2] = [1.0, 0.0];
    const NEAR: [f32; 2] = [3.0, 4.0]; // cosine 0.6 against X, exactly
    const Y: [f32; 2] = [0.0, 1.0];

    #[test]
    fn the_threshold_is_inclusive_and_groups_are_ordered_by_smallest_id() {
        let items: Vec<(i64, &[f32])> = vec![(9, &Y), (2, &X), (5, &NEAR), (4, &Y)];
        assert_eq!(cluster(&items, p(0.6, 2, 10)), vec![vec![2, 5], vec![4, 9]]);
        // NEAR is 0.8 against Y, so once X cannot take it (0.61 > 0.6) seed 4 does.
        assert_eq!(cluster(&items, p(0.61, 2, 10)), vec![vec![4, 5, 9]]);
    }
    #[test]
    fn the_result_does_not_depend_on_the_input_order() {
        let a: Vec<(i64, &[f32])> = vec![(1, &X), (2, &X), (3, &X), (4, &Y)];
        let b: Vec<(i64, &[f32])> = vec![(4, &Y), (3, &X), (2, &X), (1, &X)];
        assert_eq!(cluster(&a, p(0.9, 1, 10)), cluster(&b, p(0.9, 1, 10)));
    }
    #[test]
    fn min_size_drops_small_groups_and_max_size_cuts_the_loosest() {
        let items: Vec<(i64, &[f32])> = vec![(1, &X), (2, &NEAR), (3, &X), (4, &X)];
        assert_eq!(cluster(&items, p(0.6, 4, 10)), vec![vec![1, 2, 3, 4]]);
        assert!(cluster(&items, p(0.6, 5, 10)).is_empty());
        // Cap 3: the seed keeps its two closest (3, 4); id 2 (cosine 0.6) is left to seed alone.
        assert_eq!(cluster(&items, p(0.6, 1, 3)), vec![vec![1, 3, 4], vec![2]]);
    }
    #[test]
    fn a_vector_of_another_dimension_never_joins_a_group() {
        let odd = [1.0_f32, 0.0, 0.0];
        let items: Vec<(i64, &[f32])> = vec![(1, &X), (2, &odd)];
        assert_eq!(cluster(&items, p(0.5, 1, 10)), vec![vec![1], vec![2]]);
    }
}
