//! The in-memory pool: every account, its latest usage, its live reservations. The decision is
//! taken against a snapshot of it, under one lock, so two launches never both see the same room.
//! W0 holds the state and answers "no decision" (today's launch); the decision branch fills in
//! `decide_and_reserve`.

use std::sync::{Arc, Mutex, OnceLock};

use chrono::{DateTime, Duration, Utc};

use super::choose::{Decision, SessionView};
use super::provider::{CacheModel, LimitHit, SessionPortability};
use super::settings::AccountPoolDoc;
use super::{Account, AccountId};

#[derive(Debug, Clone, PartialEq)]
pub struct WindowUse {
    /// Opaque: `5h`, `7d`, `1h`, `30d`, ...
    pub name: String,
    pub used_fraction: f64,
    pub resets_at: Option<DateTime<Utc>>,
    /// Parsed from the name (`<n>h` | `<n>d`); None when unparseable.
    pub length: Option<Duration>,
    pub stale: bool,
}

/// `<n>h` or `<n>d` as a duration.
pub fn window_length(name: &str) -> Option<Duration> {
    let (digits, unit) = name.split_at(name.len().checked_sub(1)?);
    let n: i64 = digits.parse().ok()?;
    match unit {
        "h" => Some(Duration::hours(n)),
        "d" => Some(Duration::days(n)),
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct AccountUsage {
    pub account_id: AccountId,
    pub fidelity: crate::quota::Fidelity,
    pub read_at: Option<DateTime<Utc>>,
    pub windows: Vec<WindowUse>,
    pub detail: String,
}

/// One account as the decision sees it.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub account: Account,
    pub usage: Option<AccountUsage>,
    /// Reservations held now.
    pub live_sessions: u32,
    /// Projection per live session per window.
    pub per_session_fraction: Vec<(String, f64)>,
    /// Windows the provider reported exhausted (from a failed launch).
    pub exhausted: Vec<(String, Option<DateTime<Utc>>)>,
}

/// Every account, in any state.
#[derive(Debug, Clone, Default)]
pub struct PoolView {
    pub candidates: Vec<Candidate>,
}

pub enum Holder {
    Run(i64),
    Chat(String),
}

pub struct PickRequest {
    pub engine: String,
    pub provider: String,
    pub session: SessionView,
    pub holder: Holder,
    pub portability: SessionPortability,
    pub cache_model: Option<CacheModel>,
}

#[derive(Default)]
struct Inner {
    candidates: Vec<Candidate>,
    next_ticket: u64,
    /// `(ticket, account)` of every live reservation.
    held: Vec<(u64, AccountId)>,
}

pub struct PoolState {
    inner: Mutex<Inner>,
}

/// A held slot on one account; dropping it releases the slot.
pub struct Reservation {
    pool: Arc<PoolState>,
    account: AccountId,
    ticket: u64,
    holder: Holder,
}

impl Reservation {
    pub fn account(&self) -> &AccountId {
        &self.account
    }
    pub fn holder(&self) -> &Holder {
        &self.holder
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        let mut inner = self.pool.lock();
        inner.held.retain(|(ticket, _)| *ticket != self.ticket);
    }
}

impl PoolState {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(Inner::default()),
        })
    }

    /// The daemon's one pool. Empty until startup loads accounts; an empty pool makes every pick
    /// `Decision::Default` (today's launch).
    pub fn global() -> Arc<Self> {
        static POOL: OnceLock<Arc<PoolState>> = OnceLock::new();
        POOL.get_or_init(PoolState::new).clone()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Replaces the account list; usage, projections and exhaustion marks of accounts that stay
    /// are kept.
    pub fn set_accounts(&self, accounts: Vec<Account>) {
        let mut inner = self.lock();
        let mut old = std::mem::take(&mut inner.candidates);
        inner.candidates = accounts
            .into_iter()
            .map(
                |account| match old.iter().position(|c| c.account.id == account.id) {
                    Some(at) => Candidate {
                        account,
                        ..old.swap_remove(at)
                    },
                    None => Candidate {
                        account,
                        usage: None,
                        live_sessions: 0,
                        per_session_fraction: Vec::new(),
                        exhausted: Vec::new(),
                    },
                },
            )
            .collect();
    }

    pub fn set_usage(&self, usage: Vec<AccountUsage>) {
        let mut inner = self.lock();
        for candidate in &mut inner.candidates {
            candidate.usage = usage
                .iter()
                .find(|u| u.account_id == candidate.account.id)
                .cloned();
        }
    }

    pub fn set_projection(&self, account: &AccountId, window: &str, per_session_fraction: f64) {
        let mut inner = self.lock();
        if let Some(c) = inner
            .candidates
            .iter_mut()
            .find(|c| &c.account.id == account)
        {
            match c.per_session_fraction.iter_mut().find(|(w, _)| w == window) {
                Some(slot) => slot.1 = per_session_fraction,
                None => c
                    .per_session_fraction
                    .push((window.to_owned(), per_session_fraction)),
            }
        }
    }

    pub fn mark_exhausted(&self, account: &AccountId, hit: &LimitHit, now: DateTime<Utc>) {
        let mut inner = self.lock();
        if let Some(c) = inner
            .candidates
            .iter_mut()
            .find(|c| &c.account.id == account)
        {
            // Marks whose reset has passed no longer hold.
            c.exhausted
                .retain(|(_, resets)| resets.is_none_or(|at| at > now));
            let window = hit.window.clone().unwrap_or_default();
            c.exhausted.retain(|(w, _)| *w != window);
            c.exhausted.push((window, hit.resets_at));
        }
    }

    pub fn view(&self) -> PoolView {
        let inner = self.lock();
        PoolView {
            candidates: inner
                .candidates
                .iter()
                .map(|c| Candidate {
                    live_sessions: Self::count(&inner, &c.account.id),
                    ..c.clone()
                })
                .collect(),
        }
    }

    fn count(inner: &Inner, account: &AccountId) -> u32 {
        inner.held.iter().filter(|(_, a)| a == account).count() as u32
    }

    pub fn live_sessions(&self, account: &AccountId) -> u32 {
        Self::count(&self.lock(), account)
    }

    /// Takes a slot on `account` for `holder`. For the decision branch, which calls it while
    /// deciding; a reservation is only ever made through the pool that owns it.
    pub fn reserve(self: &Arc<Self>, account: &AccountId, holder: Holder) -> Reservation {
        let mut inner = self.lock();
        inner.next_ticket += 1;
        let ticket = inner.next_ticket;
        inner.held.push((ticket, account.clone()));
        Reservation {
            pool: self.clone(),
            account: account.clone(),
            ticket,
            holder,
        }
    }

    /// Snapshot, decision and reservation under one lock. W0: no decision, so today's launch.
    pub fn decide_and_reserve(
        self: &Arc<Self>,
        _req: &PickRequest,
        _doc: &AccountPoolDoc,
        _now: DateTime<Utc>,
    ) -> (Decision, Option<Reservation>) {
        (Decision::Default, None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accounts::{AccountKind, AccountState};

    fn account(id: &str) -> Account {
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

    #[test]
    fn window_names_parse_to_lengths() {
        assert_eq!(window_length("1h"), Some(Duration::hours(1)));
        assert_eq!(window_length("30d"), Some(Duration::days(30)));
        assert_eq!(window_length("weekly"), None);
        assert_eq!(window_length(""), None);
    }

    #[test]
    fn reservations_count_and_release_on_drop() {
        let pool = PoolState::new();
        pool.set_accounts(vec![account("fake:1")]);
        let id = AccountId("fake:1".into());
        let held = pool.reserve(&id, Holder::Run(7));
        assert_eq!(pool.live_sessions(&id), 1);
        assert_eq!(pool.view().candidates[0].live_sessions, 1);
        drop(held);
        assert_eq!(pool.live_sessions(&id), 0);
    }

    #[test]
    fn an_empty_pool_decides_the_default_launch() {
        let pool = PoolState::new();
        let req = PickRequest {
            engine: "claude".into(),
            provider: "anthropic".into(),
            session: SessionView::default(),
            holder: Holder::Chat("c".into()),
            portability: SessionPortability::NotPortable,
            cache_model: None,
        };
        let (decision, reservation) =
            pool.decide_and_reserve(&req, &AccountPoolDoc::default(), Utc::now());
        assert_eq!(decision, Decision::Default);
        assert!(reservation.is_none());
    }

    #[test]
    fn set_accounts_keeps_the_state_of_accounts_that_stay() {
        let pool = PoolState::new();
        let id = AccountId("fake:1".into());
        pool.set_accounts(vec![account("fake:1")]);
        pool.set_projection(&id, "1h", 0.1);
        pool.set_accounts(vec![account("fake:1"), account("fake:2")]);
        let view = pool.view();
        assert_eq!(view.candidates.len(), 2);
        assert_eq!(
            view.candidates[0].per_session_fraction,
            vec![("1h".to_owned(), 0.1)]
        );
    }
}
