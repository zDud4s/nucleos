//! The one autonomy ladder behind every retry limit of the tree driver (autonomy-trees spec B1).
//!
//! Each limit used to be a constant of its own in the module that spent it (`wrap` WRAPPED / DIED,
//! `watchdog` FAILURE_CAP, `driver` FIX_CAP / CONFLICT_ATTEMPTS, `rescue` one rescue, `retry`
//! GATE_ATTEMPTS / LAND_ATTEMPTS, and the 6 h wait cap of a starved gate). Each site still persists
//! its own count, as before, and asks this module what comes next:
//!
//! 1. retry, after a backoff of minutes where the site has a way to wait ([`Step::Retry`]);
//! 2. once the kind's cap of retries is spent, one fresh "diagnose and decide" run
//!    ([`Step::Diagnose`]): the node is started again with the evidence of the failures (a rescue
//!    run, see `rescue::attempt`), and what it does is applied as the answer;
//! 3. only after that run was spent too, park ([`Step::Park`]): the owner is asked.
//!
//! A site that has no agent to start for its kind (a gate that measured nothing, a landing, a
//! relaunch that cannot even spawn) treats [`Step::Diagnose`] as a park.

use std::time::Duration;

/// Which limit is being spent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Kind {
    /// Relaunches that follow a `handoff` report (`wrap`).
    Wrapped,
    /// Relaunches of a run that died without a report (`wrap`).
    Died,
    /// Failed relaunches of a node in a row (`watchdog`).
    WatchdogFailure,
    /// Relaunches that follow a red gate (`driver`).
    Fix,
    /// Merge requests the conflict resolver is given (`driver`).
    Conflict,
    /// The diagnose run itself: no retries, one run per allowance (`rescue`).
    Rescue,
    /// Gates of a branch in a row that measured nothing (`retry`).
    Gate,
    /// Landings of a job that failed in the queue or were refused (`retry`).
    Land,
    /// The accumulated wait of a gate starved by infrastructure: a time cap, not a count
    /// (spec A1).
    GateLoadWait,
}

/// How long a starved gate may wait in total before it leaves rung 1 (autonomy-trees spec A1).
pub(super) const LOAD_CAP_SECS: i64 = 6 * 3600;

/// What a site has already spent of one kind since the owner's last answer.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct Spent {
    /// Retries already made after the first failure.
    pub retries: i64,
    /// Seconds already waited (only [`Kind::GateLoadWait`] reads it).
    pub waited_secs: i64,
    /// Whether the diagnose run of this allowance was already spent.
    pub diagnosed: bool,
}

/// What a site does next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Step {
    /// Try again once `after` has passed.
    Retry { after: Duration },
    /// Start the one diagnose-and-decide run.
    Diagnose,
    /// Ask the owner.
    Park,
}

/// The retries of a kind before its diagnose rung, counted after the first failure. The old caps
/// were 2-3 attempts; every count-based kind is now 4-5.
pub(super) const fn retry_cap(kind: Kind) -> i64 {
    match kind {
        Kind::Wrapped => 5,
        Kind::Died
        | Kind::WatchdogFailure
        | Kind::Fix
        | Kind::Conflict
        | Kind::Gate
        | Kind::Land => 4,
        // The diagnose run is the rung after the retries: it has none of its own.
        Kind::Rescue => 0,
        // Bounded by time, see `LOAD_CAP_SECS`.
        Kind::GateLoadWait => i64::MAX,
    }
}

/// `(first, longest)` wait in seconds: the first is doubled per retry already made.
const fn backoff_secs(kind: Kind) -> (u64, u64) {
    match kind {
        // A handoff is progress, not a failure; a fix reacts to new evidence; the landing queue
        // and the resolver pace themselves. None of them waits.
        Kind::Wrapped | Kind::Fix | Kind::Conflict | Kind::Land | Kind::Rescue => (0, 0),
        Kind::Died | Kind::Gate => (2 * 60, 30 * 60),
        Kind::WatchdogFailure => (60, 30 * 60),
        Kind::GateLoadWait => (30, 10 * 60),
    }
}

/// The wait before the retry that follows `retries` earlier ones: the kind's first wait, doubled per
/// retry, never more than its longest.
pub(super) fn backoff(kind: Kind, retries: i64) -> Duration {
    let (first, longest) = backoff_secs(kind);
    let doubled = first.saturating_mul(1_u64 << retries.clamp(0, 20));
    Duration::from_secs(doubled.min(longest))
}

/// The next rung for `kind` given what it has `spent`.
pub(super) fn next_step(kind: Kind, spent: Spent) -> Step {
    let after = backoff(kind, spent.retries);
    let retry = match kind {
        Kind::GateLoadWait => {
            let next = spent
                .waited_secs
                .saturating_add(i64::try_from(after.as_secs()).unwrap_or(i64::MAX));
            next <= LOAD_CAP_SECS
        }
        _ => spent.retries < retry_cap(kind),
    };
    if retry {
        Step::Retry { after }
    } else if spent.diagnosed {
        Step::Park
    } else {
        Step::Diagnose
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [Kind; 9] = [
        Kind::Wrapped,
        Kind::Died,
        Kind::WatchdogFailure,
        Kind::Fix,
        Kind::Conflict,
        Kind::Rescue,
        Kind::Gate,
        Kind::Land,
        Kind::GateLoadWait,
    ];

    fn spent(retries: i64) -> Spent {
        Spent {
            retries,
            ..Spent::default()
        }
    }

    #[test]
    fn every_count_kind_retries_four_or_five_times_before_the_diagnose_rung() {
        for kind in [
            Kind::Wrapped,
            Kind::Died,
            Kind::WatchdogFailure,
            Kind::Fix,
            Kind::Conflict,
            Kind::Gate,
            Kind::Land,
        ] {
            let cap = retry_cap(kind);
            assert!((4..=5).contains(&cap), "{kind:?}: {cap}");
            for retries in 0..cap {
                assert!(
                    matches!(next_step(kind, spent(retries)), Step::Retry { .. }),
                    "{kind:?} retry {retries}"
                );
            }
            assert_eq!(next_step(kind, spent(cap)), Step::Diagnose, "{kind:?}");
        }
    }

    #[test]
    fn the_caps_per_kind_are_the_documented_numbers() {
        assert_eq!(retry_cap(Kind::Wrapped), 5);
        assert_eq!(retry_cap(Kind::Died), 4);
        assert_eq!(retry_cap(Kind::WatchdogFailure), 4);
        assert_eq!(retry_cap(Kind::Fix), 4);
        assert_eq!(retry_cap(Kind::Conflict), 4);
        assert_eq!(retry_cap(Kind::Gate), 4);
        assert_eq!(retry_cap(Kind::Land), 4);
        assert_eq!(retry_cap(Kind::Rescue), 0);
    }

    #[test]
    fn a_spent_diagnose_run_leaves_only_the_park() {
        for kind in ALL {
            let spent = Spent {
                retries: retry_cap(kind).min(1000),
                waited_secs: LOAD_CAP_SECS,
                diagnosed: true,
            };
            assert_eq!(next_step(kind, spent), Step::Park, "{kind:?}");
        }
    }

    #[test]
    fn the_diagnose_run_has_no_retries_of_its_own() {
        assert_eq!(next_step(Kind::Rescue, spent(0)), Step::Diagnose);
        let spent = Spent {
            diagnosed: true,
            ..Spent::default()
        };
        assert_eq!(next_step(Kind::Rescue, spent), Step::Park);
    }

    #[test]
    fn a_retry_never_comes_back_after_the_diagnose_rung_even_with_a_stale_count() {
        // Counts only grow between answers; a larger count than the cap is still past the cap.
        assert_eq!(next_step(Kind::Fix, spent(40)), Step::Diagnose);
        assert_eq!(
            next_step(
                Kind::Fix,
                Spent {
                    retries: 40,
                    diagnosed: true,
                    ..Spent::default()
                }
            ),
            Step::Park
        );
    }

    #[test]
    fn backoff_is_minutes_for_failures_and_doubles_up_to_half_an_hour() {
        let minutes = |kind, retries| backoff(kind, retries).as_secs() / 60;
        assert_eq!(
            (0..6).map(|n| minutes(Kind::Died, n)).collect::<Vec<_>>(),
            [2, 4, 8, 16, 30, 30]
        );
        assert_eq!(
            (0..6).map(|n| minutes(Kind::Gate, n)).collect::<Vec<_>>(),
            [2, 4, 8, 16, 30, 30]
        );
        assert_eq!(
            (0..7)
                .map(|n| backoff(Kind::WatchdogFailure, n).as_secs())
                .collect::<Vec<_>>(),
            [60, 120, 240, 480, 960, 1800, 1800]
        );
    }

    #[test]
    fn kinds_that_react_to_new_evidence_do_not_wait() {
        for kind in [
            Kind::Wrapped,
            Kind::Fix,
            Kind::Conflict,
            Kind::Land,
            Kind::Rescue,
        ] {
            for retries in 0..8 {
                assert_eq!(backoff(kind, retries), Duration::ZERO, "{kind:?}");
            }
        }
        assert_eq!(
            next_step(Kind::Fix, spent(0)),
            Step::Retry {
                after: Duration::ZERO
            }
        );
    }

    #[test]
    fn backoff_survives_absurd_counts() {
        for kind in ALL {
            let _ = backoff(kind, i64::MAX);
            let _ = backoff(kind, -3);
        }
        assert_eq!(backoff(Kind::Died, i64::MAX), Duration::from_secs(1800));
    }

    #[test]
    fn a_starved_gate_waits_30s_doubling_to_10_minutes() {
        let secs = |waits| backoff(Kind::GateLoadWait, waits).as_secs();
        assert_eq!(
            (0..7).map(secs).collect::<Vec<_>>(),
            [30, 60, 120, 240, 480, 600, 600]
        );
    }

    #[test]
    fn a_starved_gate_leaves_the_retry_rung_only_when_the_wait_would_pass_six_hours() {
        let waiting = |waits, waited_secs| {
            next_step(
                Kind::GateLoadWait,
                Spent {
                    retries: waits,
                    waited_secs,
                    diagnosed: false,
                },
            )
        };
        assert_eq!(
            waiting(0, 0),
            Step::Retry {
                after: Duration::from_secs(30)
            }
        );
        // Ten minutes still fit exactly; one second more does not.
        assert_eq!(
            waiting(9, LOAD_CAP_SECS - 600),
            Step::Retry {
                after: Duration::from_secs(600)
            }
        );
        assert_eq!(waiting(9, LOAD_CAP_SECS - 599), Step::Diagnose);
        assert_eq!(waiting(9, LOAD_CAP_SECS), Step::Diagnose);
    }

    #[test]
    fn the_waits_of_a_starved_gate_add_up_to_just_under_six_hours() {
        let mut waited = 0;
        let mut waits = 0;
        while let Step::Retry { after } = next_step(
            Kind::GateLoadWait,
            Spent {
                retries: waits,
                waited_secs: waited,
                diagnosed: false,
            },
        ) {
            waited += after.as_secs() as i64;
            waits += 1;
        }
        assert!(waited <= LOAD_CAP_SECS);
        assert!(waited > LOAD_CAP_SECS - 600, "{waited}");
    }
}
