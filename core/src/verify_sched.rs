//! Picks the next verification unit to start. Pure: the executor feeds it the queue,
//! the free capacity and when each project last started something.
// Nothing outside the tests calls this until the F2a executor does; core is a binary crate, so
// clippy's `dead_code` would otherwise fail the gate.
#![cfg_attr(not(test), allow(dead_code))]

use std::collections::HashMap;

/// A queued request, as far as scheduling cares.
pub(crate) struct Candidate<'a> {
    pub id: i64,
    pub project: Option<&'a str>,
    pub priority: i64,
    pub weight: i64,
    pub enqueued_ms: i64,
}

/// The priority a candidate competes at: one level better once it waited `aging_ms`.
pub(crate) fn effective_priority(c: &Candidate, now_ms: i64, aging_ms: i64) -> i64 {
    if now_ms.saturating_sub(c.enqueued_ms) >= aging_ms {
        (c.priority - 1).max(0)
    } else {
        c.priority
    }
}

/// The id to start now, or `None` when the queue is empty or its head does not fit.
///
/// Order: best effective priority, then the project whose last start is oldest (never started
/// goes first), then FIFO by `enqueued_ms`, then `id`. A head that does not fit blocks the
/// queue: nothing behind it jumps ahead, so heavy units are not starved.
pub(crate) fn pick(
    candidates: &[Candidate],
    free: i64,
    last_start: &HashMap<Option<String>, u64>,
    now_ms: i64,
    aging_ms: i64,
) -> Option<i64> {
    let winner = candidates.iter().min_by_key(|c| {
        let key = c.project.map(str::to_owned);
        // `None` (never started) orders before any `Some`.
        let started = last_start.get(&key).copied();
        (
            effective_priority(c, now_ms, aging_ms),
            started,
            c.enqueued_ms,
            c.id,
        )
    })?;
    (winner.weight <= free).then_some(winner.id)
}

#[cfg(test)]
mod tests {
    use super::*;

    const AGING: i64 = 1_000;

    fn cand(id: i64, project: &str, priority: i64, enqueued_ms: i64) -> Candidate<'_> {
        Candidate {
            id,
            project: Some(project),
            priority,
            weight: 1,
            enqueued_ms,
        }
    }

    fn starts(pairs: &[(&str, u64)]) -> HashMap<Option<String>, u64> {
        pairs
            .iter()
            .map(|(p, n)| (Some((*p).to_owned()), *n))
            .collect()
    }

    #[test]
    fn the_interactive_request_goes_first() {
        let cs = [cand(1, "a", 2, 0), cand(2, "a", 0, 10), cand(3, "a", 1, 5)];
        assert_eq!(pick(&cs, 4, &HashMap::new(), 10, AGING), Some(2));
    }

    #[test]
    fn an_aged_request_climbs_one_level_and_no_more() {
        let aged = cand(1, "a", 2, 0);
        assert_eq!(effective_priority(&aged, 1_000, AGING), 1);
        assert_eq!(effective_priority(&aged, 999, AGING), 2);
        // Competes as 1, not 0: a fresh interactive request still beats it.
        let cs = [aged, cand(2, "a", 0, 5_000)];
        assert_eq!(pick(&cs, 4, &HashMap::new(), 5_000, AGING), Some(2));
        // And it ties with a fresh priority-1 request, then loses on FIFO order only if later.
        let cs = [cand(3, "a", 2, 0), cand(4, "a", 1, 5_000)];
        assert_eq!(pick(&cs, 4, &HashMap::new(), 5_000, AGING), Some(3));
    }

    #[test]
    fn aging_never_goes_below_the_top_level() {
        let c = cand(1, "a", 0, 0);
        assert_eq!(effective_priority(&c, 1_000_000, AGING), 0);
    }

    #[test]
    fn projects_take_turns_within_a_level() {
        let cs = [
            cand(1, "a", 1, 0),
            cand(2, "a", 1, 1),
            cand(3, "a", 1, 2),
            cand(4, "b", 1, 3),
        ];
        // A started last (higher counter), so B goes next despite being enqueued last.
        let ls = starts(&[("a", 5), ("b", 2)]);
        assert_eq!(pick(&cs, 4, &ls, 10, AGING), Some(4));
    }

    #[test]
    fn a_project_that_never_started_goes_before_one_that_did() {
        let cs = [cand(1, "a", 1, 0), cand(2, "b", 1, 5)];
        let ls = starts(&[("a", 1)]);
        assert_eq!(pick(&cs, 4, &ls, 10, AGING), Some(2));
    }

    #[test]
    fn fifo_within_a_project() {
        let cs = [cand(2, "a", 1, 7), cand(1, "a", 1, 3), cand(3, "a", 1, 3)];
        // Same enqueue time falls back to the lower id.
        assert_eq!(pick(&cs, 4, &HashMap::new(), 10, AGING), Some(1));
    }

    #[test]
    fn a_head_that_does_not_fit_blocks_the_queue() {
        let mut heavy = cand(1, "a", 1, 0);
        heavy.weight = 2;
        let light = cand(2, "a", 1, 1);
        let cs = [heavy, light];
        assert_eq!(pick(&cs, 1, &HashMap::new(), 10, AGING), None);
        assert_eq!(pick(&cs, 2, &HashMap::new(), 10, AGING), Some(1));
    }

    #[test]
    fn an_empty_queue_picks_nothing() {
        assert_eq!(pick(&[], 4, &HashMap::new(), 10, AGING), None);
    }
}
