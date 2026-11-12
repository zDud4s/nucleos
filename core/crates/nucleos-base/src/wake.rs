//! Level-triggered change hints: "something of this kind may have changed, re-read it".
//! A hint carries no data and may be spurious; the database stays the source of truth.
//!
//! Process-wide on purpose: `feed::append_on` and its siblings receive only a pool or a
//! transaction, so threading a bus through them would change ~120 signatures.

use std::sync::LazyLock;
use tokio::sync::watch;

#[derive(Clone, Copy, Debug)]
pub enum Topic {
    Feed,
    Proposals,
    Kill,
    Budget,
    Runs,
}

const TOPICS: usize = 5;

static SENDERS: LazyLock<[watch::Sender<u64>; TOPICS]> =
    LazyLock::new(|| std::array::from_fn(|_| watch::Sender::new(0)));

/// Never blocks, never fails, works with no subscriber: one atomic bump.
pub fn bump(topic: Topic) {
    SENDERS[topic as usize].send_modify(|n| *n = n.wrapping_add(1));
}

/// A receiver that sees every bump made after this call as a change.
pub fn subscribe(topic: Topic) -> watch::Receiver<u64> {
    SENDERS[topic as usize].subscribe()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bump_wakes_an_earlier_subscriber() {
        let rx = subscribe(Topic::Feed);
        bump(Topic::Feed);
        assert!(rx.has_changed().unwrap());
    }

    #[test]
    fn a_later_subscriber_sees_the_current_version_not_a_change() {
        bump(Topic::Proposals);
        let rx = subscribe(Topic::Proposals);
        assert!(!rx.has_changed().unwrap());
    }

    #[tokio::test]
    async fn a_burst_coalesces_into_one_change() {
        let mut rx = subscribe(Topic::Kill);
        for _ in 0..10 {
            bump(Topic::Kill);
        }
        rx.changed().await.unwrap();
        assert!(!rx.has_changed().unwrap());
    }

    #[test]
    fn bump_without_subscribers_does_not_panic() {
        bump(Topic::Budget);
    }
}
