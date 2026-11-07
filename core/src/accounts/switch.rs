//! What moving a session to another account costs. W0 declares the shape and says "unknown", which
//! the decision treats as a high cost; the switch branch computes it.

use chrono::{DateTime, Utc};

use super::choose::SessionView;
use super::pool::Candidate;
use super::provider::CacheModel;

/// Cost of moving a session, in window-fraction units of the destination.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SwitchCost {
    /// The cache has expired: nothing is lost.
    Zero,
    Fraction(f64),
    /// Treated as high.
    Unknown,
}

pub fn switch_cost(
    _session: &SessionView,
    _cache: Option<CacheModel>,
    _destination: &Candidate,
    _now: DateTime<Utc>,
) -> SwitchCost {
    SwitchCost::Unknown
}
