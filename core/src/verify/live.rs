//! The live view of `capacity` and `unit_timeout_seconds` for the verify executor.
//!
//! Each read asks a [`Source`] for the current config. A valid answer becomes the new last-good
//! value; an invalid one is logged and ignored; no answer keeps what was there.

use std::sync::{Arc, Mutex};

use crate::config::VerifyConfig;

/// Where the current verify config comes from; `None` means "nothing newer than what you have".
pub(crate) type Source = Arc<dyn Fn() -> Option<VerifyConfig> + Send + Sync>;

#[derive(Clone, Copy)]
struct Good {
    capacity: u32,
    unit_timeout_seconds: u64,
}

pub(crate) struct Limits {
    good: Mutex<Good>,
    source: Source,
}

impl Limits {
    pub(crate) fn new(initial: &VerifyConfig, source: Source) -> Self {
        Self {
            good: Mutex::new(Good {
                capacity: initial.capacity,
                unit_timeout_seconds: initial.unit_timeout_seconds,
            }),
            source,
        }
    }

    /// Pulls the source once and returns the (possibly updated) last-good values.
    fn refresh(&self) -> Good {
        // The source runs before the lock is taken, so it can never deadlock with this mutex.
        let fresh = (self.source)();
        let mut good = self
            .good
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(config) = fresh {
            match config.check() {
                Ok(()) => {
                    good.capacity = config.capacity;
                    good.unit_timeout_seconds = config.unit_timeout_seconds;
                }
                Err(error) => {
                    tracing::warn!(%error, "verify: ignoring an invalid verify setting, keeping the last good one");
                }
            }
        }
        *good
    }

    /// The machine-wide weight capacity, never below 1.
    pub(crate) fn capacity(&self) -> i64 {
        i64::from(self.refresh().capacity).max(1)
    }

    /// The default per-unit timeout, in seconds.
    pub(crate) fn unit_timeout_seconds(&self) -> u64 {
        self.refresh().unit_timeout_seconds
    }
}

/// Reads the verify config from the settings store (what `PUT /settings/machine/verify` updates).
pub(crate) fn settings_source() -> Source {
    Arc::new(|| {
        crate::settings::get_opt::<VerifyConfig>(&crate::settings::Scope::Machine)
            .map(|config| (*config).clone())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::VerifyConfig;
    use std::sync::{Arc, Mutex};

    /// A source whose answer the test swaps at will.
    fn shared(initial: Option<VerifyConfig>) -> (Arc<Mutex<Option<VerifyConfig>>>, Source) {
        let cell = Arc::new(Mutex::new(initial));
        let reader = Arc::clone(&cell);
        let source: Source = Arc::new(move || reader.lock().unwrap().clone());
        (cell, source)
    }

    fn config(capacity: u32, unit_timeout_seconds: u64) -> VerifyConfig {
        VerifyConfig {
            capacity,
            unit_timeout_seconds,
            ..VerifyConfig::default()
        }
    }

    #[test]
    fn limits_start_from_the_initial_config_when_the_source_has_nothing() {
        let (_cell, source) = shared(None);
        let limits = Limits::new(&config(5, 90), source);
        assert_eq!(limits.capacity(), 5);
        assert_eq!(limits.unit_timeout_seconds(), 90);
    }

    #[test]
    fn limits_follow_a_valid_new_config_on_the_next_call() {
        let (cell, source) = shared(None);
        let limits = Limits::new(&config(5, 90), source);
        assert_eq!(limits.capacity(), 5);
        *cell.lock().unwrap() = Some(config(2, 30));
        assert_eq!(limits.capacity(), 2);
        assert_eq!(limits.unit_timeout_seconds(), 30);
        *cell.lock().unwrap() = Some(config(8, 600));
        assert_eq!(limits.unit_timeout_seconds(), 600);
        assert_eq!(limits.capacity(), 8);
        // The source going quiet again keeps the last good value.
        *cell.lock().unwrap() = None;
        assert_eq!(limits.capacity(), 8);
        assert_eq!(limits.unit_timeout_seconds(), 600);
    }

    #[test]
    fn limits_keep_the_last_good_value_when_the_config_is_invalid() {
        let (cell, source) = shared(None);
        let limits = Limits::new(&config(5, 90), source);
        *cell.lock().unwrap() = Some(config(3, 45));
        assert_eq!(limits.capacity(), 3);
        // Capacity 0 fails `VerifyConfig::check`: the 3 and 45 stay.
        *cell.lock().unwrap() = Some(config(0, 10));
        assert_eq!(limits.capacity(), 3);
        assert_eq!(limits.unit_timeout_seconds(), 45);
        assert!(limits.capacity() >= 1);
    }
}
