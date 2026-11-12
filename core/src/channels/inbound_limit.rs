//! Rate limit for inbound events from unknown senders (sec-inbound#1). Ingest asks it before the
//! `channel_events` row is written, so a stranger flooding a bot cannot grow the database: an
//! event over the limit is only counted (`Drops::unknown_sender_limited`) and dropped. Senders that
//! own a `channel_identities` row never reach it. Memory only, and bounded: at most
//! `TRACKED_SENDERS` senders are remembered, and expired windows are evicted first.

use std::collections::HashMap;

use chrono::{DateTime, Duration, Utc};

/// Events one unknown sender may send per channel inside `WINDOW`. Pairing takes one or two.
pub const PER_SENDER_LIMIT: u32 = 5;
/// Events all unknown senders together may send per channel inside `WINDOW`.
pub const PER_CHANNEL_LIMIT: u32 = 300;
/// Both limits are fixed windows of this length, each starting at its first event.
pub const WINDOW: Duration = Duration::hours(1);
/// Senders remembered at once across every channel. Only an event that passed both limits adds a
/// sender, so the channel ceiling already keeps the map small; this is the backstop.
pub const TRACKED_SENDERS: usize = 4096;

/// A count and the start of the window it belongs to.
#[derive(Debug, Clone, Copy)]
struct Window {
    count: u32,
    since: DateTime<Utc>,
}

impl Window {
    fn fresh(now: DateTime<Utc>) -> Self {
        Self {
            count: 0,
            since: now,
        }
    }

    fn expired(&self, now: DateTime<Utc>, window: Duration) -> bool {
        now - self.since >= window
    }

    /// The count inside the current window: zero once it has expired.
    fn live_count(&self, now: DateTime<Utc>, window: Duration) -> u32 {
        if self.expired(now, window) {
            0
        } else {
            self.count
        }
    }
}

/// The limiter. `Default` uses the constants above; `with_limits` exists for tests.
#[derive(Debug)]
pub struct InboundLimit {
    per_sender: u32,
    per_channel: u32,
    window: Duration,
    tracked: usize,
    senders: HashMap<(&'static str, String), Window>,
    channels: HashMap<&'static str, Window>,
}

impl Default for InboundLimit {
    fn default() -> Self {
        Self::with_limits(PER_SENDER_LIMIT, PER_CHANNEL_LIMIT, WINDOW, TRACKED_SENDERS)
    }
}

impl InboundLimit {
    pub fn with_limits(
        per_sender: u32,
        per_channel: u32,
        window: Duration,
        tracked: usize,
    ) -> Self {
        Self {
            per_sender,
            per_channel,
            window,
            tracked,
            senders: HashMap::new(),
            channels: HashMap::new(),
        }
    }

    /// Whether one more event from this unknown sender may be recorded at `now`. A `true` charges
    /// the event to both the sender and the channel; a `false` charges nothing.
    pub fn allow(&mut self, channel: &'static str, sender: &str, now: DateTime<Utc>) -> bool {
        let window = self.window;
        let key = (channel, sender.to_string());
        let sender_count = self
            .senders
            .get(&key)
            .map_or(0, |w| w.live_count(now, window));
        if sender_count >= self.per_sender {
            return false;
        }
        let channel_count = self
            .channels
            .get(channel)
            .map_or(0, |w| w.live_count(now, window));
        if channel_count >= self.per_channel {
            return false;
        }
        if !self.senders.contains_key(&key) && self.senders.len() >= self.tracked {
            self.senders.retain(|_, w| !w.expired(now, window));
            if self.senders.len() >= self.tracked {
                // Fail closed: forgetting a live sender would hand it a fresh allowance.
                return false;
            }
        }
        charge(
            self.senders
                .entry(key)
                .or_insert_with(|| Window::fresh(now)),
            now,
            window,
        );
        charge(
            self.channels
                .entry(channel)
                .or_insert_with(|| Window::fresh(now)),
            now,
            window,
        );
        true
    }

    /// Senders currently remembered (tests and diagnostics).
    pub fn tracked_senders(&self) -> usize {
        self.senders.len()
    }
}

fn charge(entry: &mut Window, now: DateTime<Utc>, window: Duration) {
    if entry.expired(now, window) {
        *entry = Window::fresh(now);
    }
    entry.count += 1;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(minutes: i64) -> DateTime<Utc> {
        "2026-10-20T10:00:00Z".parse::<DateTime<Utc>>().unwrap() + Duration::minutes(minutes)
    }

    #[test]
    fn a_sender_past_its_limit_is_refused_until_the_window_rolls_over() {
        let mut limit = InboundLimit::with_limits(2, 100, Duration::minutes(60), 16);
        assert!(limit.allow("telegram", "s1", at(0)));
        assert!(limit.allow("telegram", "s1", at(1)));
        assert!(!limit.allow("telegram", "s1", at(2)));
        assert!(!limit.allow("telegram", "s1", at(59)));
        // Another sender and another channel have their own allowance.
        assert!(limit.allow("telegram", "s2", at(2)));
        assert!(limit.allow("fake", "s1", at(2)));
        assert!(limit.allow("telegram", "s1", at(60)));
    }

    #[test]
    fn the_channel_ceiling_caps_many_distinct_senders() {
        let mut limit = InboundLimit::with_limits(5, 3, Duration::minutes(60), 16);
        for sender in ["a", "b", "c"] {
            assert!(limit.allow("telegram", sender, at(0)));
        }
        assert!(!limit.allow("telegram", "d", at(1)));
        assert!(
            limit.allow("fake", "d", at(1)),
            "the ceiling is per channel"
        );
        assert!(limit.allow("telegram", "d", at(60)));
    }

    #[test]
    fn a_refused_event_charges_nothing() {
        let mut limit = InboundLimit::with_limits(1, 2, Duration::minutes(60), 16);
        assert!(limit.allow("telegram", "a", at(0)));
        for _ in 0..10 {
            assert!(!limit.allow("telegram", "a", at(1)));
        }
        assert!(
            limit.allow("telegram", "b", at(1)),
            "a's refusals used no channel budget"
        );
    }

    #[test]
    fn the_sender_map_stays_bounded_and_evicts_expired_windows_first() {
        let mut limit = InboundLimit::with_limits(5, 1000, Duration::minutes(60), 3);
        for sender in ["a", "b", "c"] {
            assert!(limit.allow("telegram", sender, at(0)));
        }
        assert!(
            !limit.allow("telegram", "d", at(1)),
            "full of live senders: fail closed"
        );
        assert!(
            limit.allow("telegram", "a", at(1)),
            "a known entry needs no room"
        );
        assert_eq!(limit.tracked_senders(), 3);
        assert!(
            limit.allow("telegram", "d", at(61)),
            "expired windows were evicted"
        );
        assert!(limit.tracked_senders() <= 3);
    }
}
