//! Which account takes the next turn or run. Pure: no I/O, no model call. W0 declares the types
//! and answers `Decision::Default` (today's launch); the decision branch fills in the scoring.

use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::Serialize;

use super::pool::Candidate;
use super::provider::{CacheModel, SessionPortability};
use super::settings::Weights;
use super::{AccountChoice, AccountId};

#[derive(Debug, Clone, Default)]
pub struct SessionView {
    /// None = a new session (free choice).
    pub current: Option<AccountId>,
    /// Auto | Provider | Account (the owner's pin).
    pub choice: AccountChoice,
    pub previous: Option<(AccountId, DateTime<Utc>)>,
    pub last_cache_write_at: Option<DateTime<Utc>>,
    pub cached_tokens: Option<i64>,
    pub stream_ttl: Option<Duration>,
}

pub struct ChooseInput<'a> {
    pub provider: &'a str,
    /// Only this provider's accounts.
    pub candidates: &'a [Candidate],
    pub session: &'a SessionView,
    pub weights: &'a Weights,
    pub portability: SessionPortability,
    pub mid_switch_enabled: bool,
    pub cache_model: Option<CacheModel>,
    pub now: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Reason {
    OnlyAccount,
    Pinned,
    Scored {
        score: f64,
    },
    StayedForCache,
    StayedNotPortable,
    LeastBadAllAtRisk,
    /// Stale or unmeasured usage.
    Neutral,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockReason {
    Paused,
    NeedsLogin,
    AtLimit,
    Removed,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Decision {
    /// Empty pool or an engine outside the pool: launch exactly as today, no env, no reservation.
    Default,
    Use {
        account: AccountId,
        reason: Reason,
        switched_from: Option<AccountId>,
    },
    /// The pinned account cannot take work; it is never switched silently.
    PinnedBlocked {
        pinned: AccountId,
        why: BlockReason,
        window: Option<String>,
        alternative: Option<AccountId>,
    },
    /// A not-portable session whose account is exhausted: propose a new session elsewhere.
    SessionStuck {
        current: AccountId,
        alternative: Option<AccountId>,
    },
    NoCandidate {
        why: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Score {
    pub value: f64,
    pub at_risk: bool,
    pub measured: bool,
}

/// W0: no decision, so today's launch.
pub fn choose_account(_input: &ChooseInput<'_>) -> Decision {
    Decision::Default
}

/// Per-candidate score; shared with the inter filter. W0: neutral and unmeasured.
pub fn score(_candidate: &Candidate, _weights: &Weights, _now: DateTime<Utc>) -> Score {
    Score {
        value: 0.0,
        at_risk: false,
        measured: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_skeleton_decides_the_default_launch() {
        let session = SessionView::default();
        let weights = Weights::default();
        let input = ChooseInput {
            provider: "anthropic",
            candidates: &[],
            session: &session,
            weights: &weights,
            portability: SessionPortability::NotPortable,
            mid_switch_enabled: true,
            cache_model: None,
            now: Utc::now(),
        };
        assert_eq!(choose_account(&input), Decision::Default);
        assert_eq!(session.choice, AccountChoice::Auto);
    }
}
