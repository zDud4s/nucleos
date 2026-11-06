//! What moving a session to another account costs (design §7.3, B2). Pure.
//!
//! Nothing here names a provider: the TTL and the price ratios come from the adapter's
//! `CacheModel`, the cached size from the session's own last turn.

use std::time::Duration;

use chrono::{DateTime, Utc};

use super::choose::SessionView;
use super::pool::Candidate;
use super::provider::{CacheModel, CONSERVATIVE_TTL};

/// Cost of moving a session, in window-fraction units of the destination.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SwitchCost {
    /// The cache has expired: nothing is lost.
    Zero,
    Fraction(f64),
    /// Treated as high: the session only moves for risk or after a limit error (B2).
    Unknown,
}

/// Builder choice (§13): tokens of plain input that one "reference session" of the destination
/// spends, i.e. the size at which `per_session_fraction` of a window is consumed.
pub const TOKENS_PER_REFERENCE_SESSION: f64 = 200_000.0;
/// Used when the destination has no per-session projection at all.
pub const FALLBACK_SESSION_FRACTION: f64 = 0.02;

/// `switch_cost_at` with the conservative 5 min TTL for a cache whose TTL nobody reports.
pub fn switch_cost(
    session: &SessionView,
    cache: Option<CacheModel>,
    destination: &Candidate,
    now: DateTime<Utc>,
) -> SwitchCost {
    switch_cost_at(session, cache, destination, CONSERVATIVE_TTL, now)
}

/// The cost of moving `session` to `destination`.
///
/// - the cache expired (time since the last write > TTL): `Zero`. The TTL is the stream's, else
///   the adapter's, else `default_ttl`;
/// - no recorded cache write, no `cache_model` or no cached size: `Unknown` (B2);
/// - otherwise the cached prefix times `(write price - read price)`, converted to a fraction of the
///   destination's window through its per-session projection.
pub fn switch_cost_at(
    session: &SessionView,
    cache: Option<CacheModel>,
    destination: &Candidate,
    default_ttl: Duration,
    now: DateTime<Utc>,
) -> SwitchCost {
    let Some(written) = session.last_cache_write_at else {
        return SwitchCost::Unknown;
    };
    let ttl = session
        .stream_ttl
        .or(cache.map(|model| model.ttl))
        .unwrap_or(default_ttl);
    // A clock that went backwards counts as "just written".
    let elapsed = now
        .signed_duration_since(written)
        .to_std()
        .unwrap_or_default();
    if elapsed > ttl {
        return SwitchCost::Zero;
    }
    let (Some(model), Some(tokens)) = (cache, session.cached_tokens) else {
        return SwitchCost::Unknown;
    };
    if tokens <= 0 {
        return SwitchCost::Zero;
    }
    let extra_price = (model.write_price_ratio - model.read_price_ratio).max(0.0);
    let per_session = destination
        .per_session_fraction
        .iter()
        .map(|(_, fraction)| *fraction)
        .reduce(f64::max)
        .unwrap_or(FALLBACK_SESSION_FRACTION);
    SwitchCost::Fraction(tokens as f64 * extra_price * per_session / TOKENS_PER_REFERENCE_SESSION)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accounts::choose::test_support::candidate;

    fn model() -> CacheModel {
        CacheModel {
            ttl: Duration::from_secs(600),
            write_price_ratio: 1.25,
            read_price_ratio: 0.1,
        }
    }

    fn session(age_secs: i64, tokens: Option<i64>, now: DateTime<Utc>) -> SessionView {
        SessionView {
            last_cache_write_at: Some(now - chrono::Duration::seconds(age_secs)),
            cached_tokens: tokens,
            ..SessionView::default()
        }
    }

    #[test]
    fn an_expired_cache_costs_nothing() {
        let now = Utc::now();
        let dest = candidate("fake:b", &[], now);
        let cost = switch_cost(&session(601, Some(900_000), now), Some(model()), &dest, now);
        assert_eq!(cost, SwitchCost::Zero);
    }

    #[test]
    fn a_missing_ttl_falls_back_to_five_minutes() {
        let now = Utc::now();
        let dest = candidate("fake:b", &[], now);
        assert_eq!(
            switch_cost(&session(301, None, now), None, &dest, now),
            SwitchCost::Zero
        );
        assert_eq!(
            switch_cost(&session(299, None, now), None, &dest, now),
            SwitchCost::Unknown
        );
    }

    #[test]
    fn the_stream_ttl_beats_the_adapter_default() {
        let now = Utc::now();
        let dest = candidate("fake:b", &[], now);
        let mut view = session(100, Some(1000), now);
        view.stream_ttl = Some(Duration::from_secs(60));
        assert_eq!(
            switch_cost(&view, Some(model()), &dest, now),
            SwitchCost::Zero
        );
    }

    #[test]
    fn an_unknown_cache_or_size_is_unknown() {
        let now = Utc::now();
        let dest = candidate("fake:b", &[], now);
        assert_eq!(
            switch_cost(&session(10, Some(1000), now), None, &dest, now),
            SwitchCost::Unknown
        );
        assert_eq!(
            switch_cost(&session(10, None, now), Some(model()), &dest, now),
            SwitchCost::Unknown
        );
        assert_eq!(
            switch_cost(&SessionView::default(), Some(model()), &dest, now),
            SwitchCost::Unknown
        );
    }

    #[test]
    fn a_warm_cache_costs_in_proportion_to_its_size() {
        let now = Utc::now();
        let dest = candidate("fake:b", &[], now);
        let small = switch_cost(&session(10, Some(10_000), now), Some(model()), &dest, now);
        let big = switch_cost(&session(10, Some(400_000), now), Some(model()), &dest, now);
        let (SwitchCost::Fraction(small), SwitchCost::Fraction(big)) = (small, big) else {
            panic!("a known warm cache has a fraction cost");
        };
        assert!(small > 0.0 && big > small * 39.0 && big < small * 41.0);
    }
}
