//! The inter-provider filter (design §8): which engines have no headroom left in their provider
//! pool, so `route_advice` can stop proposing them.
//!
//! STABLE CONTRACT for `h-inter-filter`: the two functions below are pure, take the pool view the
//! caller already holds (`PoolState::view()`), the settings weights and the clock, and return
//! sorted, de-duplicated names. They never name a provider or an engine themselves.

use std::collections::BTreeSet;

use chrono::{DateTime, Utc};

use super::AccountState;
use super::choose::score;
use super::pool::{Candidate, PoolView};
use super::settings::Weights;

/// An active account that can still take work: no window at or over the exhaustion threshold,
/// counting its live sessions' projected load, and no live exhaustion mark.
fn has_room(candidate: &Candidate, weights: &Weights, now: DateTime<Utc>) -> bool {
    candidate.account.state == AccountState::Active && !score(candidate, weights, now).at_risk
}

/// Providers (sorted) that still have at least one active account out of risk. A provider whose
/// accounts are all paused, in need of login or at risk is absent.
pub fn providers_with_headroom(
    view: &PoolView,
    weights: &Weights,
    now: DateTime<Utc>,
) -> Vec<String> {
    view.candidates
        .iter()
        .filter(|c| has_room(c, weights, now))
        .map(|c| c.account.provider.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// Engines (sorted) whose provider pool has no candidate out of risk, counting projected load.
/// A provider with fewer than two active accounts is never returned: with a single account the
/// existing brake alone decides (P7).
pub fn engines_without_headroom(
    view: &PoolView,
    weights: &Weights,
    now: DateTime<Utc>,
) -> Vec<String> {
    let providers: BTreeSet<&str> = view
        .candidates
        .iter()
        .map(|c| c.account.provider.as_str())
        .collect();
    let mut engines = BTreeSet::new();
    for provider in providers {
        let active: Vec<&Candidate> = view
            .candidates
            .iter()
            .filter(|c| c.account.provider == provider && c.account.state == AccountState::Active)
            .collect();
        if active.len() >= 2 && !active.iter().any(|c| has_room(c, weights, now)) {
            engines.extend(
                view.candidates
                    .iter()
                    .filter(|c| c.account.provider == provider)
                    .map(|c| c.account.engine.clone()),
            );
        }
    }
    engines.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accounts::choose::test_support::{DAY, candidate};

    fn view(candidates: Vec<Candidate>) -> PoolView {
        PoolView { candidates }
    }

    #[test]
    fn an_empty_pool_has_no_engine_without_headroom() {
        assert!(
            engines_without_headroom(&PoolView::default(), &Weights::default(), Utc::now())
                .is_empty()
        );
        assert!(
            providers_with_headroom(&PoolView::default(), &Weights::default(), Utc::now())
                .is_empty()
        );
    }

    #[test]
    fn a_provider_with_every_account_at_risk_has_no_headroom() {
        let now = Utc::now();
        let w = Weights::default();
        let a = candidate("fake:a", &[("1h", 0.97, 600)], now);
        let b = candidate("fake:b", &[("30d", 0.96, DAY)], now);
        let v = view(vec![a.clone(), b.clone()]);
        assert_eq!(
            engines_without_headroom(&v, &w, now),
            vec!["fake".to_owned()]
        );
        assert!(providers_with_headroom(&v, &w, now).is_empty());
        // One account with room is enough.
        let roomy = candidate("fake:b", &[("30d", 0.2, DAY)], now);
        let v = view(vec![a, roomy]);
        assert!(engines_without_headroom(&v, &w, now).is_empty());
        assert_eq!(
            providers_with_headroom(&v, &w, now),
            vec!["fakeco".to_owned()]
        );
    }

    #[test]
    fn a_single_active_account_is_never_filtered() {
        let now = Utc::now();
        let only = candidate("fake:a", &[("1h", 0.99, 600)], now);
        let mut paused = candidate("fake:b", &[("1h", 0.0, 600)], now);
        paused.account.state = AccountState::Paused;
        let v = view(vec![only, paused]);
        assert!(engines_without_headroom(&v, &Weights::default(), now).is_empty());
    }

    #[test]
    fn projected_load_of_live_sessions_counts() {
        let now = Utc::now();
        let mut a = candidate("fake:a", &[("1h", 0.5, 600)], now);
        let mut b = candidate("fake:b", &[("1h", 0.5, 600)], now);
        a.live_sessions = 30;
        b.live_sessions = 30;
        let v = view(vec![a, b]);
        assert_eq!(
            engines_without_headroom(&v, &Weights::default(), now),
            vec!["fake".to_owned()]
        );
    }

    #[test]
    fn unmeasured_accounts_count_as_headroom() {
        let now = Utc::now();
        let a = candidate("fake:a", &[("1h", 0.99, 600)], now);
        let b = candidate("fake:b", &[], now);
        let v = view(vec![a, b]);
        assert!(engines_without_headroom(&v, &Weights::default(), now).is_empty());
    }
}
