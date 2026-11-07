//! The inter-provider filter: which engines have no headroom left in their provider pool.

use chrono::{DateTime, Utc};

use super::pool::PoolView;
use super::settings::Weights;

/// Engines whose provider pool has no candidate out of risk, counting projected load. A provider
/// with a single active account is never returned (today's brake alone decides). W0: none.
pub fn engines_without_headroom(
    _view: &PoolView,
    _weights: &Weights,
    _now: DateTime<Utc>,
) -> Vec<String> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_pool_has_no_engine_without_headroom() {
        assert!(
            engines_without_headroom(&PoolView::default(), &Weights::default(), Utc::now())
                .is_empty()
        );
    }
}
