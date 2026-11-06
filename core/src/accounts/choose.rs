//! Which account takes the next turn or run. Pure: no I/O, no model call (design §7, R3).
//!
//! Nothing here names a provider or a window: windows are opaque names with a parsed length and a
//! reset time, so the same arithmetic serves `5h/7d`, `1h/30d` or anything an adapter reports.

use std::cmp::Ordering;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::Serialize;

use super::pool::{Candidate, WindowUse};
use super::provider::{CacheModel, SessionPortability};
use super::settings::Weights;
use super::switch::{SwitchCost, switch_cost_at};
use super::{AccountChoice, AccountId, AccountState};
use crate::quota::{Fidelity, QUOTA_FRESH_FOR};

/// A window this close (as a fraction) below the exhaustion threshold starts to cost score.
pub const NEAR_RISK_BAND: f64 = 0.15;
/// Floor of the time-to-reset / window-length ratio, so an imminent reset cannot divide by ~0.
pub const MIN_RESET_FRACTION: f64 = 0.01;
/// Builder choice (§13): score units one window fraction of switch cost is worth. A switch that
/// burns 5% of the destination's window weighs one unit of score.
pub const COST_SCORE_PER_FRACTION: f64 = 20.0;

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

impl Score {
    /// Candidates compare by tier first, so the R4 guarantees hold whatever the weights are:
    /// out of risk and well measured > unmeasured or stale > at risk.
    fn tier(&self) -> u8 {
        if self.at_risk {
            0
        } else if !self.measured {
            1
        } else {
            2
        }
    }
}

type Scored<'a> = (&'a Candidate, Score);

/// Greater = better. Total: ties fall to fewer live sessions, then to the lower id.
fn rank_cmp(a: &Scored<'_>, b: &Scored<'_>) -> Ordering {
    a.1.tier()
        .cmp(&b.1.tier())
        .then(a.1.value.partial_cmp(&b.1.value).unwrap_or(Ordering::Equal))
        .then(b.0.live_sessions.cmp(&a.0.live_sessions))
        .then(b.0.account.id.cmp(&a.0.account.id))
}

fn best<'a>(scored: &[Scored<'a>]) -> Option<Scored<'a>> {
    scored.iter().copied().max_by(rank_cmp)
}

fn per_session(candidate: &Candidate, window: &str, weights: &Weights) -> f64 {
    candidate
        .per_session_fraction
        .iter()
        .find(|(name, _)| name == window)
        .map_or(weights.per_session_fraction, |(_, fraction)| *fraction)
}

/// Projected extra load of the live sessions on one window.
fn projected(candidate: &Candidate, window: &str, weights: &Weights) -> f64 {
    f64::from(candidate.live_sessions) * per_session(candidate, window, weights)
}

/// The exhaustion mark that still holds: the provider said "limit" and the reset has not passed.
fn active_mark(candidate: &Candidate, now: DateTime<Utc>) -> Option<&String> {
    candidate
        .exhausted
        .iter()
        .find(|(_, resets)| resets.is_none_or(|at| at > now))
        .map(|(window, _)| window)
}

/// The windows when the reading can be trusted (R4): a real source, read within
/// `QUOTA_FRESH_FOR`, no window stale or already past its reset.
fn trusted_windows(candidate: &Candidate, now: DateTime<Utc>) -> Option<&[WindowUse]> {
    let usage = candidate.usage.as_ref()?;
    if usage.fidelity == Fidelity::Unmeasured || usage.windows.is_empty() {
        return None;
    }
    let fresh = usage
        .read_at
        .is_some_and(|read| now.signed_duration_since(read) <= QUOTA_FRESH_FOR);
    let current = usage
        .windows
        .iter()
        .all(|w| !w.stale && w.resets_at.is_none_or(|at| at > now));
    (fresh && current).then_some(usage.windows.as_slice())
}

/// The window an account is out of right now, if any: a provider-reported mark, else a trusted
/// reading at the exhaustion threshold. Projection is not counted: this is "cannot run", not
/// "might not finish".
pub fn exhausted_window(
    candidate: &Candidate,
    weights: &Weights,
    now: DateTime<Utc>,
) -> Option<String> {
    if let Some(window) = active_mark(candidate, now) {
        return Some(window.clone());
    }
    trusted_windows(candidate, now)?
        .iter()
        .find(|w| w.used_fraction >= weights.exhausted_at)
        .map(|w| w.name.clone())
}

/// Per-candidate score; shared with the inter filter.
///
/// Measured: the limiting window (the one with the least room once the live sessions' projected
/// load is added) gives the urgency `remaining / (time to reset / window length)`, the capacity
/// that resets soonest being used first (D4); `ln(1 + urgency)` keeps it on a scale where the
/// risk penalty and the hysteresis mean something. Near the exhaustion threshold a penalty grows.
/// At risk: `used + projected >= exhausted_at` on any window, or a live exhaustion mark; its value
/// is minus the load, so the least loaded of several at-risk accounts is the least bad.
/// Stale or unmeasured: neutral (value 0), never at risk unless marked.
pub fn score(candidate: &Candidate, weights: &Weights, now: DateTime<Utc>) -> Score {
    let marked = active_mark(candidate, now).is_some();
    let Some(windows) = trusted_windows(candidate, now) else {
        return Score {
            value: if marked { -1.0 } else { 0.0 },
            at_risk: marked,
            measured: false,
        };
    };
    let mut at_risk = marked;
    let mut limiting: Option<(f64, f64)> = None; // (load, urgency)
    for window in windows {
        let load = (window.used_fraction + projected(candidate, &window.name, weights)).max(0.0);
        at_risk |= load >= weights.exhausted_at;
        let remaining = (1.0 - load).max(0.0);
        let reset_fraction = match (window.resets_at, window.length) {
            (Some(at), Some(length)) if length.num_seconds() > 0 => {
                let left = at.signed_duration_since(now).num_seconds() as f64;
                (left / length.num_seconds() as f64).clamp(MIN_RESET_FRACTION, 1.0)
            }
            _ => 1.0,
        };
        let urgency = remaining / reset_fraction;
        if limiting.is_none_or(|(worst, _)| load > worst) {
            limiting = Some((load, urgency));
        }
    }
    let (load, urgency) = limiting.unwrap_or((0.0, 0.0));
    let value = if at_risk {
        -load - if marked { 1.0 } else { 0.0 }
    } else {
        let proximity =
            ((load - (weights.exhausted_at - NEAR_RISK_BAND)) / NEAR_RISK_BAND).clamp(0.0, 1.0);
        weights.urgency * urgency.ln_1p() - weights.risk_penalty * proximity
    };
    Score {
        value,
        at_risk,
        measured: true,
    }
}

fn reason_for(score: &Score) -> Reason {
    if score.at_risk {
        Reason::LeastBadAllAtRisk
    } else if !score.measured {
        Reason::Neutral
    } else {
        Reason::Scored { score: score.value }
    }
}

/// The decision for one launch (§7.1). Pure.
pub fn choose_account(input: &ChooseInput<'_>) -> Decision {
    if input.candidates.is_empty() {
        return Decision::Default;
    }
    let scored: Vec<Scored<'_>> = input
        .candidates
        .iter()
        .filter(|c| c.account.state == AccountState::Active)
        .map(|c| (c, score(c, input.weights, input.now)))
        .collect();
    match &input.session.choice {
        AccountChoice::Account(id) => {
            // A pin on an account that no longer exists falls back to Auto (B5).
            if let Some(pinned) = input.candidates.iter().find(|c| &c.account.id == id) {
                return choose_pinned(input, pinned, &scored);
            }
        }
        AccountChoice::Provider(provider) if provider != input.provider => {
            return Decision::NoCandidate {
                why: format!("the choice names provider `{provider}`, not `{}`", input.provider),
            };
        }
        _ => {}
    }
    choose_auto(input, &scored)
}

/// R1/R2: a pinned account is used while it can run, and never replaced silently.
fn choose_pinned(input: &ChooseInput<'_>, pinned: &Candidate, scored: &[Scored<'_>]) -> Decision {
    let id = &pinned.account.id;
    let alternative = || {
        let others: Vec<Scored<'_>> = scored
            .iter()
            .copied()
            .filter(|(c, s)| &c.account.id != id && !s.at_risk)
            .collect();
        best(&others).map(|(c, _)| c.account.id.clone())
    };
    let blocked = |why, window| Decision::PinnedBlocked {
        pinned: id.clone(),
        why,
        window,
        alternative: alternative(),
    };
    match pinned.account.state {
        AccountState::Paused => return blocked(BlockReason::Paused, None),
        AccountState::NeedsLogin => return blocked(BlockReason::NeedsLogin, None),
        AccountState::Active => {}
    }
    if let Some(window) = exhausted_window(pinned, input.weights, input.now) {
        return blocked(BlockReason::AtLimit, Some(window).filter(|w| !w.is_empty()));
    }
    Decision::Use {
        account: id.clone(),
        reason: Reason::Pinned,
        switched_from: input.session.current.clone().filter(|cur| cur != id),
    }
}

fn choose_auto(input: &ChooseInput<'_>, scored: &[Scored<'_>]) -> Decision {
    let Some(top) = best(scored) else {
        return Decision::NoCandidate {
            why: "no active account for this provider".into(),
        };
    };
    let Some(current) = input.session.current.as_ref() else {
        return use_new(top, scored.len());
    };
    let weights = input.weights;
    let movable = input.mid_switch_enabled && input.portability != SessionPortability::NotPortable;
    let alternative = || {
        let others: Vec<Scored<'_>> = scored
            .iter()
            .copied()
            .filter(|(c, s)| &c.account.id != current && !s.at_risk)
            .collect();
        best(&others).map(|(c, _)| c.account.id.clone())
    };
    let Some(cur) = scored.iter().copied().find(|(c, _)| &c.account.id == current) else {
        // The session's account cannot take work any more (paused, needs login, removed).
        return if movable {
            Decision::Use {
                account: top.0.account.id.clone(),
                reason: reason_for(&top.1),
                switched_from: Some(current.clone()),
            }
        } else {
            Decision::SessionStuck {
                current: current.clone(),
                alternative: alternative(),
            }
        };
    };
    let cur_limit = exhausted_window(cur.0, weights, input.now).is_some();
    if !movable {
        return if cur_limit {
            Decision::SessionStuck {
                current: current.clone(),
                alternative: alternative(),
            }
        } else {
            Decision::Use {
                account: current.clone(),
                reason: Reason::StayedNotPortable,
                switched_from: None,
            }
        };
    }
    if scored.len() == 1 {
        return use_new(cur, 1);
    }
    let stay = |reason| Decision::Use {
        account: current.clone(),
        reason,
        switched_from: None,
    };
    let go = Decision::Use {
        account: top.0.account.id.clone(),
        reason: reason_for(&top.1),
        switched_from: Some(current.clone()),
    };
    if top.0.account.id == cur.0.account.id || rank_cmp(&top, &cur) != Ordering::Greater {
        return stay(reason_for(&cur.1));
    }
    if cur.1.at_risk {
        // Leaving a doomed account is worth any cache; but not for another doomed one, unless the
        // provider has said the current one is out and the other has not.
        let better_exit = !top.1.at_risk || (cur_limit && exhausted_window(top.0, weights, input.now).is_none());
        return if better_exit { go } else { stay(Reason::LeastBadAllAtRisk) };
    }
    let ttl = input
        .session
        .stream_ttl
        .or(input.cache_model.map(|m| m.ttl))
        .unwrap_or(Duration::from_secs(weights.default_cache_ttl_secs));
    let mut threshold = match input.session.previous.as_ref() {
        Some((prev, at))
            if *prev == top.0.account.id
                && now_within(input.now, *at, ttl) =>
        {
            weights.hysteresis
        }
        _ => 0.0,
    };
    match switch_cost_at(
        input.session,
        input.cache_model,
        top.0,
        Duration::from_secs(weights.default_cache_ttl_secs),
        input.now,
    ) {
        SwitchCost::Zero => {}
        SwitchCost::Fraction(f) => threshold += f * COST_SCORE_PER_FRACTION,
        SwitchCost::Unknown => return stay(Reason::StayedForCache),
    }
    let gain = top.1.value - cur.1.value;
    // A better tier (measured over neutral) always counts as a gain worth more than zero.
    if gain > threshold || (threshold == 0.0 && rank_cmp(&top, &cur) == Ordering::Greater) {
        go
    } else if threshold > 0.0 {
        stay(Reason::StayedForCache)
    } else {
        stay(reason_for(&cur.1))
    }
}

fn now_within(now: DateTime<Utc>, then: DateTime<Utc>, ttl: Duration) -> bool {
    // A clock that went backwards counts as "just now".
    match now.signed_duration_since(then).to_std() {
        Ok(elapsed) => elapsed <= ttl,
        Err(_) => true,
    }
}

/// A free choice of `top`; with a single eligible account it is simply that one (R7).
fn use_new(top: Scored<'_>, eligible: usize) -> Decision {
    Decision::Use {
        account: top.0.account.id.clone(),
        reason: if eligible == 1 {
            Reason::OnlyAccount
        } else {
            reason_for(&top.1)
        },
        switched_from: None,
    }
}

/// Fixtures shared by this branch's tests: windows are `1h`/`30d`, the engine is `fake`.
#[cfg(test)]
pub(crate) mod test_support {
    use chrono::{DateTime, Duration, Utc};

    use super::super::pool::{AccountUsage, Candidate, WindowUse, window_length};
    use super::super::{Account, AccountId, AccountKind, AccountState};
    use crate::quota::Fidelity;

    pub fn account(id: &str) -> Account {
        Account {
            id: AccountId(id.into()),
            engine: "fake".into(),
            provider: "fakeco".into(),
            label: id.into(),
            kind: AccountKind::Subscription,
            credential_locator: None,
            state: AccountState::Active,
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    /// `(window name, used fraction, seconds until it resets)`.
    pub fn usage(id: &str, windows: &[(&str, f64, i64)], now: DateTime<Utc>) -> AccountUsage {
        AccountUsage {
            account_id: AccountId(id.into()),
            fidelity: Fidelity::Official,
            read_at: Some(now),
            windows: windows
                .iter()
                .map(|(name, used, resets_in)| WindowUse {
                    name: (*name).to_owned(),
                    used_fraction: *used,
                    resets_at: Some(now + Duration::seconds(*resets_in)),
                    length: window_length(name),
                    stale: false,
                })
                .collect(),
            detail: String::new(),
        }
    }

    /// A measured candidate; no windows = no usage at all (unmeasured).
    pub fn candidate(id: &str, windows: &[(&str, f64, i64)], now: DateTime<Utc>) -> Candidate {
        Candidate {
            account: account(id),
            usage: (!windows.is_empty()).then(|| usage(id, windows, now)),
            live_sessions: 0,
            per_session_fraction: Vec::new(),
            exhausted: Vec::new(),
        }
    }

    pub const DAY: i64 = 86_400;
}

#[cfg(test)]
mod tests {
    use super::test_support::*;
    use super::*;
    use crate::accounts::AccountKind;

    fn cache() -> CacheModel {
        CacheModel {
            ttl: Duration::from_secs(600),
            write_price_ratio: 1.25,
            read_price_ratio: 0.1,
        }
    }

    fn pick(
        candidates: &[Candidate],
        session: &SessionView,
        portability: SessionPortability,
        now: DateTime<Utc>,
    ) -> Decision {
        choose_account(&ChooseInput {
            provider: "fakeco",
            candidates,
            session,
            weights: &Weights::default(),
            portability,
            mid_switch_enabled: true,
            cache_model: Some(cache()),
            now,
        })
    }

    fn new_session(candidates: &[Candidate], now: DateTime<Utc>) -> Decision {
        pick(candidates, &SessionView::default(), SessionPortability::NotPortable, now)
    }

    fn account_of(decision: &Decision) -> &str {
        match decision {
            Decision::Use { account, .. } => account.as_str(),
            other => panic!("expected Use, got {other:?}"),
        }
    }

    fn on(current: &str) -> SessionView {
        SessionView {
            current: Some(AccountId(current.into())),
            ..SessionView::default()
        }
    }

    #[test]
    fn an_empty_candidate_list_decides_the_default_launch() {
        let now = Utc::now();
        assert_eq!(new_session(&[], now), Decision::Default);
    }

    #[test]
    fn p1_the_capacity_that_resets_soonest_goes_first() {
        let now = Utc::now();
        // A: more used, but resets in 2 days of 30; B: less used, resets in 24 days.
        let a = candidate("fake:a", &[("30d", 0.6, 2 * DAY)], now);
        let b = candidate("fake:b", &[("30d", 0.2, 24 * DAY)], now);
        assert_eq!(account_of(&new_session(&[b.clone(), a.clone()], now)), "fake:a");
        assert_eq!(account_of(&new_session(&[a, b], now)), "fake:a");
    }

    #[test]
    fn p1_holds_for_the_short_window_too() {
        let now = Utc::now();
        let a = candidate("fake:a", &[("1h", 0.6, 600)], now);
        let b = candidate("fake:b", &[("1h", 0.2, 3000)], now);
        assert_eq!(account_of(&new_session(&[a, b], now)), "fake:a");
    }

    #[test]
    fn p2_no_new_session_goes_over_the_threshold_while_another_has_room() {
        let now = Utc::now();
        // A would score best by urgency but is over the threshold.
        let a = candidate("fake:a", &[("30d", 0.96, DAY)], now);
        let b = candidate("fake:b", &[("30d", 0.5, 29 * DAY)], now);
        assert_eq!(account_of(&new_session(&[a.clone(), b.clone()], now)), "fake:b");
        // A reported exhausted by a failed launch: at risk until its reset.
        let mut marked = candidate("fake:a", &[("30d", 0.1, DAY)], now);
        marked.exhausted.push(("1h".into(), Some(now + chrono::Duration::hours(1))));
        assert_eq!(account_of(&new_session(&[marked.clone(), b.clone()], now)), "fake:b");
        // Once the reset has passed the mark no longer holds.
        let later = now + chrono::Duration::hours(2);
        assert!(!score(&marked, &Weights::default(), later).at_risk);
        // Projected load counts: B with many live sessions is at risk although its reading is low.
        let mut loaded = b;
        loaded.live_sessions = 50;
        assert!(score(&loaded, &Weights::default(), now).at_risk);
    }

    #[test]
    fn all_at_risk_picks_the_least_loaded() {
        let now = Utc::now();
        let a = candidate("fake:a", &[("1h", 0.99, 600)], now);
        let b = candidate("fake:b", &[("1h", 0.96, 600)], now);
        let decision = new_session(&[a, b], now);
        assert_eq!(account_of(&decision), "fake:b");
        assert!(matches!(
            decision,
            Decision::Use { reason: Reason::LeastBadAllAtRisk, .. }
        ));
    }

    #[test]
    fn stale_or_unmeasured_is_neutral_never_above_measured_with_room_never_below_at_risk() {
        let now = Utc::now();
        let measured = candidate("fake:m", &[("30d", 0.5, 20 * DAY)], now);
        let unmeasured = candidate("fake:u", &[], now);
        let at_risk = candidate("fake:r", &[("30d", 0.97, DAY)], now);
        let mut stale = candidate("fake:s", &[("30d", 0.0, DAY)], now);
        stale.usage.as_mut().unwrap().read_at = Some(now - chrono::Duration::minutes(11));
        for neutral in [&unmeasured, &stale] {
            let s = score(neutral, &Weights::default(), now);
            assert!(!s.measured && !s.at_risk);
            let picked = new_session(&[neutral.clone(), measured.clone()], now);
            assert_eq!(account_of(&picked), "fake:m");
            let picked = new_session(&[at_risk.clone(), neutral.clone()], now);
            assert_eq!(account_of(&picked), neutral.account.id.as_str());
        }
    }

    #[test]
    fn paused_and_needs_login_accounts_are_never_picked() {
        let now = Utc::now();
        let mut a = candidate("fake:a", &[("30d", 0.0, DAY)], now);
        a.account.state = AccountState::Paused;
        let mut b = candidate("fake:b", &[("30d", 0.0, DAY)], now);
        b.account.state = AccountState::NeedsLogin;
        let c = candidate("fake:c", &[("30d", 0.9, 29 * DAY)], now);
        assert_eq!(account_of(&new_session(&[a.clone(), b.clone(), c], now)), "fake:c");
        assert!(matches!(new_session(&[a, b], now), Decision::NoCandidate { .. }));
    }

    #[test]
    fn p7_a_single_account_is_used_whatever_its_state_of_usage() {
        let now = Utc::now();
        let only = candidate("fake:a", &[("1h", 0.99, 600)], now);
        assert!(matches!(
            new_session(&[only], now),
            Decision::Use { reason: Reason::OnlyAccount, .. }
        ));
        let unmeasured = candidate("fake:a", &[], now);
        assert!(matches!(
            new_session(&[unmeasured], now),
            Decision::Use { reason: Reason::OnlyAccount, .. }
        ));
    }

    #[test]
    fn p5_a_pinned_session_never_moves_on_its_own() {
        let now = Utc::now();
        let a = candidate("fake:a", &[("30d", 0.9, 29 * DAY)], now);
        let b = candidate("fake:b", &[("30d", 0.1, DAY)], now);
        let mut session = on("fake:a");
        session.choice = AccountChoice::Account(AccountId("fake:a".into()));
        let decision = pick(&[a.clone(), b.clone()], &session, SessionPortability::Portable, now);
        assert_eq!(
            decision,
            Decision::Use {
                account: AccountId("fake:a".into()),
                reason: Reason::Pinned,
                switched_from: None
            }
        );
        // Exhausted: blocked, with the best alternative named, never swapped.
        let mut out = a.clone();
        out.exhausted.push(("1h".into(), None));
        let decision = pick(&[out, b.clone()], &session, SessionPortability::Portable, now);
        assert_eq!(
            decision,
            Decision::PinnedBlocked {
                pinned: AccountId("fake:a".into()),
                why: BlockReason::AtLimit,
                window: Some("1h".into()),
                alternative: Some(AccountId("fake:b".into())),
            }
        );
        // Paused and needs-login are blocked too.
        let mut paused = a.clone();
        paused.account.state = AccountState::Paused;
        assert!(matches!(
            pick(&[paused, b.clone()], &session, SessionPortability::Portable, now),
            Decision::PinnedBlocked { why: BlockReason::Paused, .. }
        ));
        let mut login = a;
        login.account.state = AccountState::NeedsLogin;
        assert!(matches!(
            pick(&[login, b], &session, SessionPortability::Portable, now),
            Decision::PinnedBlocked { why: BlockReason::NeedsLogin, .. }
        ));
    }

    #[test]
    fn a_pin_on_a_removed_account_returns_the_session_to_auto() {
        let now = Utc::now();
        let b = candidate("fake:b", &[("30d", 0.1, DAY)], now);
        let c = candidate("fake:c", &[("30d", 0.5, 20 * DAY)], now);
        let mut session = SessionView::default();
        session.choice = AccountChoice::Account(AccountId("fake:gone".into()));
        let decision = pick(&[b, c], &session, SessionPortability::NotPortable, now);
        assert_eq!(account_of(&decision), "fake:b");
    }

    #[test]
    fn a_provider_choice_for_another_provider_has_no_candidate() {
        let now = Utc::now();
        let mut session = SessionView::default();
        session.choice = AccountChoice::Provider("otherco".into());
        let a = candidate("fake:a", &[("30d", 0.1, DAY)], now);
        assert!(matches!(
            pick(&[a], &session, SessionPortability::NotPortable, now),
            Decision::NoCandidate { .. }
        ));
    }

    #[test]
    fn a_not_portable_session_stays_until_its_account_is_out() {
        let now = Utc::now();
        let a = candidate("fake:a", &[("30d", 0.9, 29 * DAY)], now);
        let b = candidate("fake:b", &[("30d", 0.1, DAY)], now);
        let stays = pick(&[a.clone(), b.clone()], &on("fake:a"), SessionPortability::NotPortable, now);
        assert!(matches!(
            stays,
            Decision::Use { reason: Reason::StayedNotPortable, ref account, .. } if account.as_str() == "fake:a"
        ));
        let mut out = a;
        out.exhausted.push(("30d".into(), None));
        assert_eq!(
            pick(&[out, b], &on("fake:a"), SessionPortability::NotPortable, now),
            Decision::SessionStuck {
                current: AccountId("fake:a".into()),
                alternative: Some(AccountId("fake:b".into())),
            }
        );
    }

    fn warm(mut session: SessionView, now: DateTime<Utc>, age_secs: i64, tokens: i64) -> SessionView {
        session.last_cache_write_at = Some(now - chrono::Duration::seconds(age_secs));
        session.cached_tokens = Some(tokens);
        session
    }

    #[test]
    fn p4_a_warm_large_cache_stays_unless_at_risk_and_an_expired_one_moves() {
        let now = Utc::now();
        let a = candidate("fake:a", &[("30d", 0.3, 20 * DAY)], now);
        let b = candidate("fake:b", &[("30d", 0.1, 2 * DAY)], now);
        let big = warm(on("fake:a"), now, 10, 2_000_000);
        let decision = pick(&[a.clone(), b.clone()], &big, SessionPortability::Portable, now);
        assert!(matches!(
            decision,
            Decision::Use { reason: Reason::StayedForCache, ref account, switched_from: None } if account.as_str() == "fake:a"
        ));
        // Unknown size counts as high.
        let mut unknown = on("fake:a");
        unknown.last_cache_write_at = Some(now - chrono::Duration::seconds(10));
        let decision = pick(&[a.clone(), b.clone()], &unknown, SessionPortability::Portable, now);
        assert_eq!(account_of(&decision), "fake:a");
        // The account is at risk: leave whatever the cache.
        let risky = candidate("fake:a", &[("30d", 0.96, 20 * DAY)], now);
        let decision = pick(&[risky, b.clone()], &big, SessionPortability::Portable, now);
        assert!(matches!(
            decision,
            Decision::Use { ref account, switched_from: Some(_), .. } if account.as_str() == "fake:b"
        ));
        // Past the TTL (600 s) the cache costs nothing: go to the best.
        let cold = warm(on("fake:a"), now, 700, 2_000_000);
        let decision = pick(&[a, b], &cold, SessionPortability::Portable, now);
        assert!(matches!(
            decision,
            Decision::Use { ref account, switched_from: Some(_), .. } if account.as_str() == "fake:b"
        ));
    }

    #[test]
    fn p6_no_flip_flop_back_within_a_cache_life_unless_the_gain_exceeds_the_hysteresis() {
        let now = Utc::now();
        // On B, came from A a minute ago; A is only slightly better.
        let a = candidate("fake:a", &[("30d", 0.30, 10 * DAY)], now);
        let b = candidate("fake:b", &[("30d", 0.31, 10 * DAY)], now);
        let mut session = warm(on("fake:b"), now, 60, 1_000);
        session.previous = Some((AccountId("fake:a".into()), now - chrono::Duration::seconds(60)));
        let decision = pick(&[a.clone(), b.clone()], &session, SessionPortability::Portable, now);
        assert_eq!(account_of(&decision), "fake:b");
        // A is much better: the gain pays for the switch back.
        let a_better = candidate("fake:a", &[("30d", 0.05, DAY)], now);
        let decision = pick(&[a_better, b], &session, SessionPortability::Portable, now);
        assert!(matches!(
            decision,
            Decision::Use { ref account, switched_from: Some(_), .. } if account.as_str() == "fake:a"
        ));
    }

    #[test]
    fn a_session_whose_account_is_gone_moves_if_portable_and_is_stuck_otherwise() {
        let now = Utc::now();
        let b = candidate("fake:b", &[("30d", 0.1, DAY)], now);
        let c = candidate("fake:c", &[("30d", 0.5, 20 * DAY)], now);
        let moved = pick(&[b.clone(), c.clone()], &on("fake:a"), SessionPortability::SharedStore, now);
        assert!(matches!(
            moved,
            Decision::Use { ref account, switched_from: Some(_), .. } if account.as_str() == "fake:b"
        ));
        assert!(matches!(
            pick(&[b, c], &on("fake:a"), SessionPortability::NotPortable, now),
            Decision::SessionStuck { .. }
        ));
    }

    #[test]
    fn mid_switch_off_behaves_like_not_portable() {
        let now = Utc::now();
        let a = candidate("fake:a", &[("30d", 0.3, 20 * DAY)], now);
        let b = candidate("fake:b", &[("30d", 0.1, 2 * DAY)], now);
        let input = ChooseInput {
            provider: "fakeco",
            candidates: &[a, b],
            session: &on("fake:a"),
            weights: &Weights::default(),
            portability: SessionPortability::Portable,
            mid_switch_enabled: false,
            cache_model: None,
            now,
        };
        assert!(matches!(
            choose_account(&input),
            Decision::Use { reason: Reason::StayedNotPortable, .. }
        ));
    }

    #[test]
    fn equal_accounts_tie_break_on_live_sessions_then_id() {
        let now = Utc::now();
        let a = candidate("fake:a", &[], now);
        let mut b = candidate("fake:b", &[], now);
        assert_eq!(account_of(&new_session(&[b.clone(), a.clone()], now)), "fake:a");
        let mut busy_a = a;
        busy_a.live_sessions = 1;
        b.live_sessions = 0;
        assert_eq!(account_of(&new_session(&[busy_a, b], now)), "fake:b");
        let _ = AccountKind::ApiKey;
    }
}
