//! The arithmetic of a deliberation: Borda scores, how far the ballots agree, when another round
//! is worth paying for, and how many calls a council may make at most.
//!
//! Pure on purpose. Nothing here touches the database, a clock or a runner, so every rule the
//! spec (2026-10-02, §2) states as a number is pinned by a test that runs in microseconds, and the
//! orchestration that calls it cannot quietly bend one.

// The call-ceiling test spells its sums term by term — `3 + 3 + 0 + 2` reads as "N answers, R
// rankings, R-1 revisions, chairman and retry" — and the `+ 0` is the zero revisions of one round.
// Clippy calls that a no-op; here it is the arithmetic being documented.
#![cfg_attr(test, allow(clippy::identity_op))]

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// A tau at or above this reads as `strong`. The spec's number, pinned by a test.
pub const STRONG_AT: f64 = 0.6;
/// A tau at or above this (and below `STRONG_AT`) reads as `split`; below it, `none`.
pub const SPLIT_AT: f64 = 0.2;

pub const LEVEL_STRONG: &str = "strong";
pub const LEVEL_SPLIT: &str = "split";
pub const LEVEL_NONE: &str = "none";
pub const LEVEL_INSUFFICIENT: &str = "insufficient";

/// Fewer ballots than this are two opinions, not a council's, and agreement is not measured.
const MIN_BALLOTS: usize = 3;

/// One seat's Borda result.
///
/// `score` is the MEAN of the points the seat received, not the sum: a seat shown on fewer
/// ballots (because a voter abstained or cast a one-answer ballot) must not look worse for it.
/// `n` is how many ballots scored the seat, so a reader can tell 1.0 from one vote from 1.0 from
/// four.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BordaRow {
    pub seat_idx: usize,
    pub score: f64,
    pub n: usize,
}

/// How far the ballots agree, as a pairwise Kendall tau over every comparison two voters both
/// made explicitly.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Agreement {
    /// `None` when there is nothing to measure — see `level == "insufficient"`.
    pub tau: Option<f64>,
    /// One of `strong`, `split`, `none`, `insufficient`.
    pub level: String,
    /// Ballots that could rank anything (the voter was shown at least two answers).
    pub ballots: usize,
    /// Voter-pair x seat-pair comparisons that entered tau.
    pub comparisons: usize,
}

/// One usable ballot: the seats the voter was shown and the seats it ranked explicitly, in order.
struct Ballot {
    /// Seats the voter was shown — every seat but its own.
    shown: Vec<usize>,
    /// Seats the voter ranked, best first, restricted to `shown` and without repeats.
    ranked: Vec<usize>,
}

/// Reads every ballot against the labels the voter could have been shown.
///
/// The voter's own label and a label that names no seat are dropped rather than counted: neither
/// was on the voter's ballot paper, so neither can carry a position on it. A ballot shown fewer
/// than two answers ranks nothing and is left out entirely — and `k - 1` would divide by zero.
fn usable_ballots(
    ballots: &BTreeMap<usize, Vec<String>>,
    anon_map: &BTreeMap<String, usize>,
) -> Vec<Ballot> {
    ballots
        .iter()
        .filter_map(|(voter, labels)| {
            let shown: Vec<usize> = anon_map
                .values()
                .copied()
                .filter(|seat| seat != voter)
                .collect();
            if shown.len() < 2 {
                return None;
            }
            let mut ranked: Vec<usize> = Vec::new();
            for label in labels {
                if let Some(seat) = anon_map.get(label)
                    && seat != voter
                    && !ranked.contains(seat)
                {
                    ranked.push(*seat);
                }
            }
            Some(Ballot { shown, ranked })
        })
        .collect()
}

/// Borda, normalised per ballot: on a ballot of `k` shown answers position `p` earns
/// `(k-1-p)/(k-1)`, so every ballot spends the same 0..1 scale whatever the council's size.
///
/// Answers a voter was shown but did not rank share equally the mean of the positions nobody
/// filled: leaving them out says "below what I ranked", not "tied last". Sorted best first, ties
/// broken on the seat index — never on the label, which is a per-council shuffle.
pub fn borda(
    ballots: &BTreeMap<usize, Vec<String>>,
    anon_map: &BTreeMap<String, usize>,
) -> Vec<BordaRow> {
    let mut totals: BTreeMap<usize, (f64, usize)> =
        anon_map.values().map(|seat| (*seat, (0.0, 0))).collect();

    for ballot in usable_ballots(ballots, anon_map) {
        let k = ballot.shown.len();
        let span = (k - 1) as f64;
        let points = |position: usize| (k - 1 - position) as f64 / span;

        for (position, seat) in ballot.ranked.iter().enumerate() {
            let entry = totals.entry(*seat).or_insert((0.0, 0));
            entry.0 += points(position);
            entry.1 += 1;
        }
        let filled = ballot.ranked.len();
        let unfilled = k - filled;
        if unfilled > 0 {
            let share = (filled..k).map(points).sum::<f64>() / unfilled as f64;
            for seat in ballot.shown.iter().filter(|s| !ballot.ranked.contains(s)) {
                let entry = totals.entry(*seat).or_insert((0.0, 0));
                entry.0 += share;
                entry.1 += 1;
            }
        }
    }

    let mut rows: Vec<BordaRow> = totals
        .into_iter()
        .map(|(seat_idx, (sum, n))| BordaRow {
            seat_idx,
            score: if n == 0 { 0.0 } else { sum / n as f64 },
            n,
        })
        .collect();
    rows.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then(a.seat_idx.cmp(&b.seat_idx))
    });
    rows
}

/// Pairwise Kendall tau across voters.
///
/// For every unordered pair of seats and every unordered pair of voters that BOTH ranked those
/// two seats explicitly, one comparison: concordant when both put the same seat first. A pair a
/// voter left tied in its unranked remainder is no comparison at all — neither agreement nor
/// disagreement. `tau = 2c/m - 1`; the level reads a negative tau as `none`.
pub fn agreement(
    ballots: &BTreeMap<usize, Vec<String>>,
    anon_map: &BTreeMap<String, usize>,
) -> Agreement {
    let usable = usable_ballots(ballots, anon_map);
    let positions: Vec<BTreeMap<usize, usize>> = usable
        .iter()
        .map(|b| {
            b.ranked
                .iter()
                .enumerate()
                .map(|(p, seat)| (*seat, p))
                .collect()
        })
        .collect();

    let seats: Vec<usize> = anon_map
        .values()
        .copied()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let mut comparisons = 0usize;
    let mut concordant = 0usize;
    for (i, x) in seats.iter().enumerate() {
        for y in &seats[i + 1..] {
            // Each voter's verdict on (x, y), when it ranked both: true when x is above y.
            let verdicts: Vec<bool> = positions
                .iter()
                .filter_map(|pos| Some(pos.get(x)? < pos.get(y)?))
                .collect();
            for (u, first) in verdicts.iter().enumerate() {
                for second in &verdicts[u + 1..] {
                    comparisons += 1;
                    if first == second {
                        concordant += 1;
                    }
                }
            }
        }
    }

    let tau = if usable.len() < MIN_BALLOTS || comparisons == 0 {
        None
    } else {
        Some(2.0 * concordant as f64 / comparisons as f64 - 1.0)
    };
    let level = match tau {
        None => LEVEL_INSUFFICIENT,
        Some(t) if t.max(0.0) >= STRONG_AT => LEVEL_STRONG,
        Some(t) if t.max(0.0) >= SPLIT_AT => LEVEL_SPLIT,
        Some(_) => LEVEL_NONE,
    };
    Agreement {
        tau,
        level: level.to_string(),
        ballots: usable.len(),
        comparisons,
    }
}

/// The seats whose answer drew at least one `disagree`, from critiques keyed by the critic's seat,
/// each naming the answer it judged by its anonymous label.
///
/// A label that names no seat, or the critic's own, is ignored for the same reason `borda`
/// ignores it: the critic was never shown it.
#[allow(dead_code)] // Read by the early-stop rule of the revise rounds, council-deliberacao P7.
pub fn contested(
    critiques: &BTreeMap<usize, Vec<(String, bool)>>,
    anon_map: &BTreeMap<String, usize>,
) -> BTreeSet<usize> {
    critiques
        .iter()
        .flat_map(|(critic, notes)| {
            notes.iter().filter_map(move |(label, disagrees)| {
                let seat = *anon_map.get(label)?;
                (*disagrees && seat != *critic).then_some(seat)
            })
        })
        .collect()
}

/// Another round is worth paying for only if it could change something. Stop when the Borda
/// order did not move AND no answer is disputed now that was not disputed before — a dispute that
/// settled is no reason to go on.
#[allow(dead_code)] // Asked between revise rounds, council-deliberacao P7.
pub fn should_stop_early(
    prev_order: &[usize],
    prev_contested: &BTreeSet<usize>,
    order: &[usize],
    contested: &BTreeSet<usize>,
) -> bool {
    prev_order == order && contested.is_subset(prev_contested)
}

/// The most runner calls a council of `members` seats over `rounds` rounds can make: N answers,
/// R rankings of N, R-1 revisions of N, one chairman and one chairman retry.
#[allow(dead_code)] // No caller until the revise rounds land, council-deliberacao P7.
pub fn call_ceiling(members: usize, rounds: u32) -> usize {
    let rounds = rounds as usize;
    members + rounds * members + rounds.saturating_sub(1) * members + 2
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeMap, BTreeSet};

    /// The label every seat is shown under, as `council::anonymize` would hand it out.
    fn labels(pairs: &[(&str, usize)]) -> BTreeMap<String, usize> {
        pairs
            .iter()
            .map(|(label, seat)| (label.to_string(), *seat))
            .collect()
    }

    /// One ballot per voting seat, keyed by the voter's seat index, best label first.
    fn cast(entries: &[(usize, &[&str])]) -> BTreeMap<usize, Vec<String>> {
        entries
            .iter()
            .map(|(voter, ranked)| (*voter, ranked.iter().map(|l| l.to_string()).collect()))
            .collect()
    }

    fn row(rows: &[BordaRow], seat: usize) -> &BordaRow {
        rows.iter()
            .find(|r| r.seat_idx == seat)
            .unwrap_or_else(|| panic!("seat {seat} has no Borda row in {rows:?}"))
    }

    fn close(actual: f64, expected: f64) -> bool {
        (actual - expected).abs() < 1e-9
    }

    fn four_seats() -> BTreeMap<String, usize> {
        labels(&[("A", 0), ("B", 1), ("C", 2), ("D", 3)])
    }

    /// Everyone agrees A > B > C > D, each voter leaving out only itself.
    fn four_in_accord() -> BTreeMap<usize, Vec<String>> {
        cast(&[
            (0, &["B", "C", "D"]),
            (1, &["A", "C", "D"]),
            (2, &["A", "B", "D"]),
            (3, &["A", "B", "C"]),
        ])
    }

    /// Four seats each shown three answers: a ballot's points run 1, 0.5, 0 — normalised by the
    /// ballot's own length, so a council of any size scores on the same 0..1 scale.
    #[test]
    fn borda_full_ballots_normalise_per_ballot() {
        let rows = borda(&four_in_accord(), &four_seats());

        let a = row(&rows, 0);
        assert!(
            close(a.score, 1.0),
            "A was first on all three ballots it was shown on: {a:?}"
        );
        assert_eq!(a.n, 3);
        let b = row(&rows, 1);
        assert!(close(b.score, (1.0 + 0.5 + 0.5) / 3.0), "{b:?}");
        assert_eq!(b.n, 3);
        let c = row(&rows, 2);
        assert!(close(c.score, (0.5 + 0.5 + 0.0) / 3.0), "{c:?}");
        assert_eq!(c.n, 3);
        let d = row(&rows, 3);
        assert!(close(d.score, 0.0), "{d:?}");
        assert_eq!(d.n, 3);

        let order: Vec<usize> = rows.iter().map(|r| r.seat_idx).collect();
        assert_eq!(order, vec![0, 1, 2, 3], "sorted by score, best first");
    }

    /// A voter that ranks only its favourite has not said the other two are equally last: they
    /// share the points of the positions nobody filled, (0.5 + 0) / 2 each. Labels the voter could
    /// not have been shown — its own, or one that names no seat — are ignored, not counted.
    #[test]
    fn borda_partial_ballot_shares_the_remaining_points() {
        let map = four_seats();
        let rows = borda(&cast(&[(0, &["B"])]), &map);

        let b = row(&rows, 1);
        assert!(close(b.score, 1.0), "{b:?}");
        assert_eq!(b.n, 1);
        for seat in [2, 3] {
            let r = row(&rows, seat);
            assert!(
                close(r.score, 0.25),
                "seat {seat} shares the remainder: {r:?}"
            );
            assert_eq!(r.n, 1, "a tied remainder still counts as a vote received");
        }

        let noisy = borda(&cast(&[(0, &["B", "nobody", "A"])]), &map);
        assert_eq!(
            noisy, rows,
            "an unknown label and the voter's own label change nothing"
        );
    }

    /// A seat that cast no ballot is still ranked by everyone else, and `n` is the number of
    /// ballots that scored a seat — not whether the seat itself voted.
    #[test]
    fn borda_abstainer_casts_nothing_and_n_counts_votes_received() {
        let map = labels(&[("A", 0), ("B", 1), ("C", 2)]);
        let rows = borda(&cast(&[(1, &["A", "C"]), (2, &["A", "B"])]), &map);

        let a = row(&rows, 0);
        assert!(close(a.score, 1.0), "{a:?}");
        assert_eq!(a.n, 2, "the abstainer received both ballots");
        let b = row(&rows, 1);
        assert!(close(b.score, 0.0), "{b:?}");
        assert_eq!(b.n, 1, "only seat 2 was shown B and voted");
        let c = row(&rows, 2);
        assert!(close(c.score, 0.0), "{c:?}");
        assert_eq!(c.n, 1, "only seat 1 was shown C and voted");
    }

    /// With two seats each voter is shown one answer: a "ranking" of one says nothing about order,
    /// and (k-1) would be a division by zero. No such ballot scores anyone.
    #[test]
    fn borda_k1_ballot_is_not_counted() {
        let map = labels(&[("A", 0), ("B", 1)]);
        let rows = borda(&cast(&[(0, &["B"]), (1, &["A"])]), &map);

        assert!(
            rows.iter().all(|r| r.n == 0),
            "a one-answer ballot casts no votes: {rows:?}"
        );
    }

    /// Labels are deliberately out of seat order: a tie broken on the label would come out A, B, C
    /// — seats 2, 0, 1 — so only a tie broken on the seat index gives 0, 1, 2.
    #[test]
    fn borda_ties_break_on_seat_idx() {
        let map = labels(&[("A", 2), ("B", 0), ("C", 1)]);
        // Seat 0 is B (shown A, C), seat 1 is C (shown A, B), seat 2 is A (shown B, C). A cycle:
        // every label is first once and last once.
        let rows = borda(
            &cast(&[(0, &["A", "C"]), (1, &["B", "A"]), (2, &["C", "B"])]),
            &map,
        );

        for r in &rows {
            assert!(close(r.score, 0.5), "a cycle ties everyone: {r:?}");
            assert_eq!(r.n, 2);
        }
        let order: Vec<usize> = rows.iter().map(|r| r.seat_idx).collect();
        assert_eq!(order, vec![0, 1, 2]);
    }

    /// Spec §2: four seats in perfect accord measure 1. Each seat pair is ranked by the two voters
    /// that are neither of them — one voter pair, six seat pairs, six comparisons.
    #[test]
    fn agreement_four_seats_in_perfect_accord_is_one() {
        let got = agreement(&four_in_accord(), &four_seats());

        assert!(
            close(got.tau.expect("four ballots measure"), 1.0),
            "{got:?}"
        );
        assert_eq!(got.level, "strong");
        assert_eq!(got.ballots, 4);
        assert_eq!(got.comparisons, 6);
    }

    /// Every one of the six comparisons runs the other way. A negative tau is not "less than no
    /// agreement" for the level: it reads as none.
    #[test]
    fn agreement_opposed_votes_is_none() {
        let ballots = cast(&[
            (0, &["B", "C", "D"]),
            (1, &["A", "D", "C"]),
            (2, &["D", "A", "B"]),
            (3, &["C", "B", "A"]),
        ]);
        let got = agreement(&ballots, &four_seats());

        assert!(
            close(got.tau.expect("four ballots measure"), -1.0),
            "{got:?}"
        );
        assert_eq!(got.level, "none");
        assert_eq!(got.comparisons, 6);
    }

    /// Seat 0 ranks only B, leaving C and D tied in the remainder. Its pairs (B,C), (B,D) and
    /// (C,D) were never ranked explicitly by it, so those comparisons do not exist — they are
    /// neither agreement nor disagreement.
    #[test]
    fn agreement_ignores_pairs_tied_in_a_partial_ballot() {
        let ballots = cast(&[
            (0, &["B"]),
            (1, &["A", "C", "D"]),
            (2, &["A", "B", "D"]),
            (3, &["A", "B", "C"]),
        ]);
        let got = agreement(&ballots, &four_seats());

        assert_eq!(
            got.comparisons, 3,
            "only (A,B), (A,C) and (A,D) remain: {got:?}"
        );
        assert!(
            close(got.tau.expect("three comparisons measure"), 1.0),
            "{got:?}"
        );
        assert_eq!(got.ballots, 4, "a partial ballot still counts as a ballot");
    }

    /// Spec §2: with three seats every pair is ranked by exactly one voter, so no two voters ever
    /// compare the same pair and there is nothing to measure.
    #[test]
    fn agreement_three_seats_is_insufficient() {
        let map = labels(&[("A", 0), ("B", 1), ("C", 2)]);
        let got = agreement(
            &cast(&[(0, &["B", "C"]), (1, &["A", "C"]), (2, &["A", "B"])]),
            &map,
        );

        assert_eq!(got.tau, None, "{got:?}");
        assert_eq!(got.level, "insufficient");
        assert_eq!(got.comparisons, 0);
        assert_eq!(got.ballots, 3);
    }

    /// Two voters CAN compare a pair here — both ranked C and D — but two ballots are not a
    /// council's opinion, and the measure refuses rather than report a tau of one comparison.
    #[test]
    fn agreement_fewer_than_three_ballots_is_insufficient() {
        let ballots = cast(&[(0, &["B", "C", "D"]), (1, &["A", "C", "D"])]);
        let got = agreement(&ballots, &four_seats());

        assert_eq!(got.tau, None, "{got:?}");
        assert_eq!(got.level, "insufficient");
        assert_eq!(got.ballots, 2);
    }

    /// The thresholds are the spec's, pinned as numbers; and a tau between them reads as split.
    #[test]
    fn agreement_thresholds_are_pinned() {
        assert_eq!(STRONG_AT, 0.6);
        assert_eq!(SPLIT_AT, 0.2);

        // The accord above with two swaps: seat 0 puts C over B, seat 1 puts D over C. Four of six
        // comparisons concordant, tau = 2*4/6 - 1 = 1/3.
        let ballots = cast(&[
            (0, &["C", "B", "D"]),
            (1, &["A", "D", "C"]),
            (2, &["A", "B", "D"]),
            (3, &["A", "B", "C"]),
        ]);
        let got = agreement(&ballots, &four_seats());
        assert!(
            close(got.tau.expect("four ballots measure"), 1.0 / 3.0),
            "{got:?}"
        );
        assert_eq!(got.level, "split");
    }

    /// Another round is worth paying for only if it could change something: the order moved, or
    /// an answer that nobody disputed before is disputed now.
    #[test]
    fn early_stop_needs_same_order_and_no_newly_contested_answer() {
        let none = BTreeSet::new();
        let one: BTreeSet<usize> = [1].into_iter().collect();
        let one_and_two: BTreeSet<usize> = [1, 2].into_iter().collect();

        assert!(should_stop_early(&[0, 1, 2], &none, &[0, 1, 2], &none));
        assert!(
            should_stop_early(&[0, 1, 2], &one_and_two, &[0, 1, 2], &one),
            "a dispute that settled is no reason to go on"
        );
        assert!(should_stop_early(&[0, 1, 2], &one, &[0, 1, 2], &one));
        assert!(
            !should_stop_early(&[0, 1, 2], &one, &[0, 1, 2], &one_and_two),
            "seat 2 is newly contested"
        );
        assert!(
            !should_stop_early(&[0, 1, 2], &none, &[1, 0, 2], &none),
            "the order moved"
        );
    }

    /// N answers, R rankings, R-1 revisions, one chairman and one chairman retry.
    #[test]
    fn call_ceiling_counts_every_phase_and_the_retry() {
        assert_eq!(call_ceiling(3, 1), 3 + 3 + 0 + 2);
        assert_eq!(call_ceiling(3, 3), 3 + 9 + 6 + 2);
        assert_eq!(call_ceiling(5, 2), 5 + 10 + 5 + 2);
    }
}
