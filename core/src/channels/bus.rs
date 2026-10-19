//! `ChannelsBus` and `Cadence` (Anexo D, D.5): what the channel loops listen to, and how often
//! they re-read when no hint arrives.

use std::time::Duration;
use tokio::sync::watch;

use crate::wake::{self, Topic};

/// What the channel tasks listen to. Not a field of `ChannelsRuntime`: `main.rs` builds it.
pub struct ChannelsBus {
    pub feed: watch::Receiver<u64>,
    pub proposals: watch::Receiver<u64>,
    pub kill: watch::Receiver<u64>,
    pub budget: watch::Receiver<u64>,
    pub runs: watch::Receiver<u64>,
}

impl ChannelsBus {
    /// The daemon's bus: one `wake::subscribe` per field.
    pub fn from_wake() -> Self {
        Self::from_receivers(
            wake::subscribe(Topic::Feed),
            wake::subscribe(Topic::Proposals),
            wake::subscribe(Topic::Kill),
            wake::subscribe(Topic::Budget),
            wake::subscribe(Topic::Runs),
        )
    }

    /// Built from explicit receivers, so a test drives each topic by hand.
    pub fn from_receivers(
        feed: watch::Receiver<u64>,
        proposals: watch::Receiver<u64>,
        kill: watch::Receiver<u64>,
        budget: watch::Receiver<u64>,
        runs: watch::Receiver<u64>,
    ) -> Self {
        Self {
            feed,
            proposals,
            kill,
            budget,
            runs,
        }
    }
}

/// Cadences, injectable so tests need no paused clock.
#[derive(Debug, Clone)]
pub struct Cadence {
    pub safety_sweep: Duration,
    pub settle: Duration,
    pub second_look: Duration,
    pub budget_min_gap: Duration,
    pub ping_after: Duration,
}

impl Default for Cadence {
    fn default() -> Self {
        Self {
            safety_sweep: Duration::from_secs(60),
            settle: Duration::from_millis(250),
            second_look: Duration::from_secs(2),
            budget_min_gap: Duration::from_secs(10),
            ping_after: Duration::from_secs(10 * 60),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cadence_defaults_are_the_documented_ones() {
        let c = Cadence::default();
        assert_eq!(c.safety_sweep, Duration::from_secs(60));
        assert_eq!(c.settle, Duration::from_millis(250));
        assert_eq!(c.second_look, Duration::from_secs(2));
        assert_eq!(c.budget_min_gap, Duration::from_secs(10));
        assert_eq!(c.ping_after, Duration::from_secs(600));
    }

    #[test]
    fn from_wake_sees_a_bump_after_it_was_built() {
        // Positive assertion only: the topic is a per-binary static (plan rule 3), and this is
        // the one test in the binary that bumps `Budget`.
        let bus = ChannelsBus::from_wake();
        wake::bump(Topic::Budget);
        assert!(bus.budget.has_changed().unwrap());
    }

    #[test]
    fn from_receivers_keeps_each_receiver_in_its_own_field() {
        let (tx, rx) = watch::channel(0u64);
        let other = || watch::channel(0u64).1;
        let bus = ChannelsBus::from_receivers(other(), other(), rx, other(), other());
        tx.send_modify(|n| *n += 1);
        assert!(bus.kill.has_changed().unwrap());
    }
}
