//! The admission book: a bound on how many heavy commands a machine LAUNCHES at once.
//!
//! What it bounds. `concurrency.rs` limits how many runs START; a run that has started can still
//! fire a build, a check and a test suite at the same moment as every other run, and a handful of
//! them saturate the machine. The book is a weighted semaphore over those launches: a heavy
//! command asks for admission, holds a weight until it finishes, and waits in strict arrival order
//! when the machine is full. Verify units take from the same pool (`try_take_verify`) and, when
//! one is starved, hold new run grants back (`set_verify_demand`).
//!
//! Why it lives in memory. A grant describes a process that is alive right now; after a restart
//! every such process is gone or unknown, so a persisted grant could only be wrong. `sweep` is the
//! backstop for the rest: it frees grants of dead runs, grants older than their ttl, and waiters
//! nobody has polled for a minute.
//!
//! Why it fails open. The book exists to protect the machine, never to stop work: a missing book,
//! a disabled one, a wait past `max_wait` and a poisoned lock all let the command through. The
//! worst outcome of a bug here is the load the machine had before the book existed.
//!
//! Time is `tokio::time::Instant`, so tests can pause and advance the clock.

pub mod heavy;
pub use heavy::weight_of_call;

use crate::config::AdmissionConfig;
use serde::Serialize;
use std::collections::{HashMap, HashSet, VecDeque};
use std::future::Future;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::Duration;
use tokio::sync::Notify;
use tokio::time::{Instant, sleep_until};

/// A verify demand nobody refreshed for this long stops holding runs back.
const VERIFY_DEMAND_TTL: Duration = Duration::from_secs(90);
/// A waiter nobody polled for this long is abandoned (its hook died).
const WAITER_POLL_TTL: Duration = Duration::from_secs(60);
/// How many runs' finished wait totals are remembered.
const FINISHED_RUNS_KEPT: usize = 1024;

/// Half the physical cores, at least one.
pub fn default_capacity() -> u32 {
    (num_cpus::get_physical() as u32 / 2).max(1)
}

/// The answer to asking for admission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Poll {
    /// The call holds `weight` units until it is released.
    Granted { weight: u32 },
    /// Still queued; `position` is 1 for the head.
    Waiting { position: usize, waited: Duration },
    /// Waited past `max_wait`: the caller lets the command through without a grant.
    GaveUp,
}

struct Grant {
    run_id: Option<i64>,
    tool_use_id: Option<String>,
    verify_id: Option<u64>,
    weight: u32,
    since: Instant,
}

impl Grant {
    fn is(&self, run_id: i64, tool_use_id: &str) -> bool {
        self.run_id == Some(run_id) && self.tool_use_id.as_deref() == Some(tool_use_id)
    }
}

struct Waiter {
    run_id: i64,
    tool_use_id: String,
    weight: u32,
    since: Instant,
    last_poll: Instant,
}

impl Waiter {
    fn is(&self, run_id: i64, tool_use_id: &str) -> bool {
        self.run_id == run_id && self.tool_use_id == tool_use_id
    }
}

struct Inner {
    grants: Vec<Grant>,
    waiters: VecDeque<Waiter>,
    finished: HashMap<i64, Duration>,
    finished_order: VecDeque<i64>,
    demand_set_at: Option<Instant>,
    next_verify_id: u64,
    verify_wake: Option<Arc<Notify>>,
}

impl Inner {
    fn used(&self) -> u32 {
        self.grants.iter().map(|grant| grant.weight).sum()
    }

    fn demand_active(&self, now: Instant) -> bool {
        self.demand_set_at
            .is_some_and(|at| now.saturating_duration_since(at) < VERIFY_DEMAND_TTL)
    }

    fn record_wait(&mut self, run_id: i64, waited: Duration) {
        if !self.finished.contains_key(&run_id) {
            self.finished_order.push_back(run_id);
            if self.finished_order.len() > FINISHED_RUNS_KEPT
                && let Some(oldest) = self.finished_order.pop_front()
            {
                self.finished.remove(&oldest);
            }
        }
        *self.finished.entry(run_id).or_default() += waited;
    }

    /// Strict FIFO: only the head may be granted, and only when it fits and no verify unit is
    /// starving. Returns how many grants it made.
    fn pump(&mut self, capacity: u32, now: Instant) -> usize {
        let mut granted = 0;
        while !self.demand_active(now) {
            let Some(head) = self.waiters.front() else {
                break;
            };
            if self.used() + head.weight > capacity {
                break;
            }
            let Some(waiter) = self.waiters.pop_front() else {
                break;
            };
            self.record_wait(waiter.run_id, now.saturating_duration_since(waiter.since));
            self.grants.push(Grant {
                run_id: Some(waiter.run_id),
                tool_use_id: Some(waiter.tool_use_id),
                verify_id: None,
                weight: waiter.weight,
                since: now,
            });
            granted += 1;
        }
        granted
    }

    fn remove_waiter(&mut self, index: usize, now: Instant) {
        if let Some(waiter) = self.waiters.remove(index) {
            self.record_wait(waiter.run_id, now.saturating_duration_since(waiter.since));
        }
    }
}

/// The book. Cheap to share behind an `Arc`; the lock is never held across an `.await`.
pub struct AdmissionBook {
    enabled: bool,
    capacity: u32,
    max_wait: Duration,
    grant_ttl: Duration,
    inner: Mutex<Inner>,
    changed: Notify,
}

impl AdmissionBook {
    pub fn new(capacity: u32, max_wait: Duration, grant_ttl: Duration) -> Self {
        Self {
            enabled: true,
            capacity: capacity.max(1),
            max_wait,
            grant_ttl,
            inner: Mutex::new(Inner {
                grants: Vec::new(),
                waiters: VecDeque::new(),
                finished: HashMap::new(),
                finished_order: VecDeque::new(),
                demand_set_at: None,
                next_verify_id: 0,
                verify_wake: None,
            }),
            changed: Notify::new(),
        }
    }

    pub fn from_config(config: &AdmissionConfig) -> Self {
        let mut book = Self::new(
            config.capacity.unwrap_or_else(default_capacity),
            Duration::from_secs(config.max_wait_seconds),
            Duration::from_secs(config.grant_ttl_seconds),
        );
        book.enabled = config.enabled;
        book
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Wake everything that may now be grantable or that waits on the book; when capacity came
    /// back, nudge the verify worker too.
    fn announce(&self, inner: &Inner, freed: bool) {
        self.changed.notify_waiters();
        if freed && let Some(wake) = &inner.verify_wake {
            wake.notify_one();
        }
    }

    fn clamp(&self, weight: u32) -> u32 {
        weight.clamp(1, self.capacity)
    }

    /// Ask for admission. Idempotent on `(run_id, tool_use_id)`, and each call counts as a poll.
    pub fn request(&self, run_id: i64, tool_use_id: &str, weight: u32) -> Poll {
        let weight = self.clamp(weight);
        if !self.enabled {
            return Poll::Granted { weight };
        }
        let now = Instant::now();
        let mut inner = self.lock();
        if let Some(grant) = inner.grants.iter().find(|g| g.is(run_id, tool_use_id)) {
            return Poll::Granted {
                weight: grant.weight,
            };
        }
        match inner.waiters.iter().position(|w| w.is(run_id, tool_use_id)) {
            Some(index) => {
                if now.saturating_duration_since(inner.waiters[index].since) >= self.max_wait {
                    inner.remove_waiter(index, now);
                    inner.pump(self.capacity, now);
                    self.announce(&inner, false);
                    return Poll::GaveUp;
                }
                inner.waiters[index].last_poll = now;
            }
            None => inner.waiters.push_back(Waiter {
                run_id,
                tool_use_id: tool_use_id.to_string(),
                weight,
                since: now,
                last_poll: now,
            }),
        }
        if inner.pump(self.capacity, now) > 0 {
            self.announce(&inner, false);
        }
        if let Some(grant) = inner.grants.iter().find(|g| g.is(run_id, tool_use_id)) {
            return Poll::Granted {
                weight: grant.weight,
            };
        }
        match inner.waiters.iter().position(|w| w.is(run_id, tool_use_id)) {
            Some(index) => Poll::Waiting {
                position: index + 1,
                waited: now.saturating_duration_since(inner.waiters[index].since),
            },
            None => Poll::GaveUp,
        }
    }

    /// When the waiter, if it still waits, reaches `max_wait`.
    fn give_up_at(&self, run_id: i64, tool_use_id: &str) -> Option<Instant> {
        self.lock()
            .waiters
            .iter()
            .find(|w| w.is(run_id, tool_use_id))
            .map(|waiter| waiter.since + self.max_wait)
    }

    /// Request, then wait until granted, given up on, or `window` has passed (still `Waiting`).
    pub async fn wait(
        &self,
        run_id: i64,
        tool_use_id: &str,
        weight: u32,
        window: Duration,
    ) -> Poll {
        let deadline = Instant::now() + window;
        loop {
            // Registered before the request, so a release between the two is not missed.
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let poll = self.request(run_id, tool_use_id, weight);
            if !matches!(poll, Poll::Waiting { .. }) || Instant::now() >= deadline {
                return poll;
            }
            let wake = self
                .give_up_at(run_id, tool_use_id)
                .map_or(deadline, |at| at.min(deadline));
            tokio::select! {
                () = &mut notified => {}
                () = sleep_until(wake) => {}
            }
        }
    }

    /// Give a call's weight back (or drop it from the queue). False when it held nothing.
    pub fn release(&self, run_id: i64, tool_use_id: &str) -> bool {
        let now = Instant::now();
        let mut inner = self.lock();
        if let Some(index) = inner.grants.iter().position(|g| g.is(run_id, tool_use_id)) {
            inner.grants.remove(index);
        } else if let Some(index) = inner.waiters.iter().position(|w| w.is(run_id, tool_use_id)) {
            inner.remove_waiter(index, now);
        } else {
            return false;
        }
        inner.pump(self.capacity, now);
        self.announce(&inner, true);
        true
    }

    /// Drop every grant and waiter of a run that ended.
    pub fn release_run(&self, run_id: i64) {
        let now = Instant::now();
        let mut inner = self.lock();
        let before = inner.grants.len() + inner.waiters.len();
        inner.grants.retain(|grant| grant.run_id != Some(run_id));
        while let Some(index) = inner.waiters.iter().position(|w| w.run_id == run_id) {
            inner.remove_waiter(index, now);
        }
        if inner.grants.len() + inner.waiters.len() != before {
            inner.pump(self.capacity, now);
            self.announce(&inner, true);
        }
    }

    /// Reclaim what nobody will release. Returns how many grants and waiters it removed.
    pub fn sweep(&self, live_runs: &HashSet<i64>) -> usize {
        let now = Instant::now();
        let ttl = self.grant_ttl;
        let mut inner = self.lock();
        let before = inner.grants.len();
        inner.grants.retain(|grant| match grant.run_id {
            // A verify grant is RAII; its Drop releases it.
            None => true,
            Some(run) => {
                live_runs.contains(&run) && now.saturating_duration_since(grant.since) < ttl
            }
        });
        let mut removed = before - inner.grants.len();
        while let Some(index) = inner.waiters.iter().position(|waiter| {
            !live_runs.contains(&waiter.run_id)
                || now.saturating_duration_since(waiter.last_poll) >= WAITER_POLL_TTL
        }) {
            inner.remove_waiter(index, now);
            removed += 1;
        }
        if removed > 0 {
            inner.pump(self.capacity, now);
            self.announce(&inner, true);
        }
        removed
    }

    /// Take verify capacity without queueing behind run waiters. A weight above capacity takes
    /// the whole capacity, or the unit would starve.
    pub fn try_take_verify(self: &Arc<Self>, weight: u32) -> Option<VerifyGrant> {
        let weight = self.clamp(weight);
        if !self.enabled {
            return Some(VerifyGrant {
                book: Arc::clone(self),
                id: None,
            });
        }
        let now = Instant::now();
        let mut inner = self.lock();
        if inner.used() + weight > self.capacity {
            return None;
        }
        let id = inner.next_verify_id;
        inner.next_verify_id += 1;
        inner.grants.push(Grant {
            run_id: None,
            tool_use_id: None,
            verify_id: Some(id),
            weight,
            since: now,
        });
        Some(VerifyGrant {
            book: Arc::clone(self),
            id: Some(id),
        })
    }

    fn release_verify(&self, id: u64) {
        let now = Instant::now();
        let mut inner = self.lock();
        let Some(index) = inner.grants.iter().position(|g| g.verify_id == Some(id)) else {
            return;
        };
        inner.grants.remove(index);
        inner.pump(self.capacity, now);
        self.announce(&inner, true);
    }

    /// The book's total capacity in units.
    pub fn capacity(&self) -> u32 {
        self.capacity
    }

    /// Units not currently held.
    pub fn free(&self) -> u32 {
        self.capacity.saturating_sub(self.lock().used())
    }

    /// A verify unit waits for capacity: hold run grants back (until 90 s after the last call).
    pub fn set_verify_demand(&self, demand: bool) {
        let now = Instant::now();
        let mut inner = self.lock();
        inner.demand_set_at = demand.then_some(now);
        if !demand && inner.pump(self.capacity, now) > 0 {
            self.announce(&inner, false);
        }
    }

    /// The verify worker to nudge whenever capacity comes back.
    pub fn set_verify_wake(&self, wake: Arc<Notify>) {
        self.lock().verify_wake = Some(wake);
    }

    /// Time a run spent queued: finished waits plus the ones still going.
    pub fn queued_time(&self, run_id: i64) -> Duration {
        let now = Instant::now();
        let inner = self.lock();
        let finished = inner.finished.get(&run_id).copied().unwrap_or_default();
        inner
            .waiters
            .iter()
            .filter(|waiter| waiter.run_id == run_id)
            .fold(finished, |total, waiter| {
                total + now.saturating_duration_since(waiter.since)
            })
    }

    pub fn snapshot(&self) -> Snapshot {
        let now = Instant::now();
        let inner = self.lock();
        Snapshot {
            enabled: self.enabled,
            capacity: self.capacity,
            used: inner.used(),
            verify_demand: inner.demand_active(now),
            holders: inner
                .grants
                .iter()
                .map(|grant| HolderView {
                    run_id: grant.run_id,
                    tool_use_id: grant.tool_use_id.clone(),
                    weight: grant.weight,
                    held_seconds: now.saturating_duration_since(grant.since).as_secs(),
                })
                .collect(),
            waiters: inner
                .waiters
                .iter()
                .enumerate()
                .map(|(index, waiter)| WaiterView {
                    run_id: waiter.run_id,
                    tool_use_id: waiter.tool_use_id.clone(),
                    weight: waiter.weight,
                    waited_seconds: now.saturating_duration_since(waiter.since).as_secs(),
                    position: index + 1,
                })
                .collect(),
        }
    }
}

/// Verify capacity held by a unit; gives its weight back when dropped.
pub struct VerifyGrant {
    book: Arc<AdmissionBook>,
    id: Option<u64>,
}

impl Drop for VerifyGrant {
    fn drop(&mut self) {
        if let Some(id) = self.id {
            self.book.release_verify(id);
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Snapshot {
    pub enabled: bool,
    pub capacity: u32,
    pub used: u32,
    pub verify_demand: bool,
    pub holders: Vec<HolderView>,
    pub waiters: Vec<WaiterView>,
}

#[derive(Debug, Clone, Serialize)]
pub struct HolderView {
    /// `None` for a verify unit.
    pub run_id: Option<i64>,
    pub tool_use_id: Option<String>,
    pub weight: u32,
    pub held_seconds: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct WaiterView {
    pub run_id: i64,
    pub tool_use_id: String,
    pub weight: u32,
    pub waited_seconds: u64,
    pub position: usize,
}

static INSTALLED: OnceLock<Arc<AdmissionBook>> = OnceLock::new();

/// Make `book` the process-wide book. The first install wins.
pub fn install(book: Arc<AdmissionBook>) {
    let _ = INSTALLED.set(book);
}

pub fn installed() -> Option<Arc<AdmissionBook>> {
    INSTALLED.get().cloned()
}

/// The wall clock ran out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WallClockElapsed;

/// Run `fut` under a wall-clock `timeout` that does not charge the time `run_id` spent queued for
/// admission: the deadline moves out by whatever the run queued since this call began.
pub async fn with_wall_clock<F: Future>(
    book: Option<&AdmissionBook>,
    run_id: i64,
    timeout: Duration,
    fut: F,
) -> Result<F::Output, WallClockElapsed> {
    let queued = || {
        book.map(|book| book.queued_time(run_id))
            .unwrap_or_default()
    };
    let start = Instant::now();
    let baseline = queued();
    let mut deadline = start + timeout;
    tokio::pin!(fut);
    loop {
        tokio::select! {
            output = &mut fut => return Ok(output),
            () = sleep_until(deadline) => {
                let extended = start + timeout + queued().saturating_sub(baseline);
                if extended <= Instant::now() {
                    return Err(WallClockElapsed);
                }
                deadline = extended;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AdmissionBook, Poll, WallClockElapsed, default_capacity, installed, with_wall_clock,
    };
    use crate::config::{ADMISSION_MAX_WAIT_CEILING_SECONDS, AdmissionConfig, parse_verify_config};
    use std::collections::HashSet;
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::time::{Instant, advance, sleep};

    fn book(capacity: u32) -> Arc<AdmissionBook> {
        Arc::new(AdmissionBook::new(
            capacity,
            Duration::from_secs(900),
            Duration::from_secs(7200),
        ))
    }

    fn is_granted(poll: &Poll) -> bool {
        matches!(poll, Poll::Granted { .. })
    }

    fn is_waiting(poll: &Poll) -> bool {
        matches!(poll, Poll::Waiting { .. })
    }

    #[tokio::test(start_paused = true)]
    async fn a_heavy_request_that_fits_is_granted_at_once() {
        let book = book(2);
        let poll = book.request(1, "a", 1);
        assert!(matches!(poll, Poll::Granted { weight: 1 }));
        assert_eq!(book.free(), 1);
        // Asking again for the same call is the same grant, not a second one.
        assert!(is_granted(&book.request(1, "a", 1)));
        assert_eq!(book.free(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn a_request_past_capacity_waits_until_a_release() {
        let book = book(1);
        assert!(is_granted(&book.request(1, "a", 1)));
        assert!(is_waiting(&book.request(2, "b", 1)));
        // Polling again keeps it in the queue once, not twice.
        assert!(is_waiting(&book.request(2, "b", 1)));
        assert_eq!(book.snapshot().waiters.len(), 1);
        assert!(book.release(1, "a"));
        assert!(is_granted(&book.request(2, "b", 1)));
        assert_eq!(book.free(), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn waiters_are_granted_in_arrival_order_even_when_a_later_one_fits() {
        let book = book(3);
        assert!(is_granted(&book.request(1, "a", 2)));
        // One unit is free: "b" needs two and waits, "c" needs one and would fit, but it is behind.
        assert!(is_waiting(&book.request(2, "b", 2)));
        assert!(is_waiting(&book.request(3, "c", 1)));
        assert_eq!(
            book.free(),
            1,
            "the later, smaller request must not jump the queue"
        );
        assert!(book.release(1, "a"));
        // Releasing runs the FIFO grant itself: "b" is the head and now holds its two units.
        assert!(is_granted(&book.request(2, "b", 2)));
        assert!(is_granted(&book.request(3, "c", 1)));
        assert_eq!(book.free(), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn a_weight_above_capacity_is_charged_the_whole_capacity() {
        let book = book(2);
        let poll = book.request(1, "a", 5);
        assert!(matches!(poll, Poll::Granted { weight: 2 }));
        assert_eq!(book.free(), 0);
        assert!(book.release(1, "a"));
        assert_eq!(book.free(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn release_is_by_tool_use_id_and_idempotent() {
        let book = book(2);
        assert!(is_granted(&book.request(1, "a", 1)));
        assert!(is_granted(&book.request(1, "b", 1)));
        assert!(!book.release(1, "nope"), "an unknown call releases nothing");
        assert_eq!(book.free(), 0);
        assert!(book.release(1, "a"));
        assert!(
            !book.release(1, "a"),
            "a second release of the same call is a no-op"
        );
        assert_eq!(
            book.free(),
            1,
            "the second release must not give back weight twice"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn release_run_frees_every_grant_and_waiter_of_the_run() {
        let book = book(2);
        assert!(is_granted(&book.request(1, "a", 1)));
        assert!(is_granted(&book.request(1, "b", 1)));
        assert!(is_waiting(&book.request(1, "c", 1)));
        assert!(is_waiting(&book.request(2, "d", 1)));
        book.release_run(1);
        let snapshot = book.snapshot();
        assert!(
            snapshot
                .holders
                .iter()
                .all(|holder| holder.run_id != Some(1))
        );
        assert!(snapshot.waiters.iter().all(|waiter| waiter.run_id != 1));
        // The other run's waiter inherits the freed capacity.
        assert!(is_granted(&book.request(2, "d", 1)));
    }

    #[tokio::test(start_paused = true)]
    async fn sweep_frees_dead_runs_expired_grants_and_abandoned_waiters() {
        let book = Arc::new(AdmissionBook::new(
            4,
            Duration::from_secs(900),
            Duration::from_secs(100),
        ));
        assert!(is_granted(&book.request(1, "dead", 1)));
        assert!(is_granted(&book.request(2, "old", 1)));
        advance(Duration::from_secs(101)).await;
        assert!(is_granted(&book.request(3, "fresh", 1)));
        let live: HashSet<i64> = [2, 3].into_iter().collect();
        assert_eq!(
            book.sweep(&live),
            2,
            "run 1 is dead and run 2's grant is past its ttl"
        );
        assert_eq!(book.free(), 3);
        assert!(!book.release(1, "dead"));
        assert!(!book.release(2, "old"));

        // A waiter nobody has polled for a minute is abandoned.
        let book = book_of(1);
        assert!(is_granted(&book.request(5, "holder", 1)));
        assert!(is_waiting(&book.request(6, "ghost", 1)));
        advance(Duration::from_secs(61)).await;
        let live: HashSet<i64> = [5, 6].into_iter().collect();
        assert_eq!(book.sweep(&live), 1);
        assert!(book.snapshot().waiters.is_empty());
    }

    /// A second, independent book inside one test.
    fn book_of(capacity: u32) -> Arc<AdmissionBook> {
        book(capacity)
    }

    #[tokio::test(start_paused = true)]
    async fn a_waiter_past_max_wait_gives_up_and_its_wait_is_recorded() {
        let book = Arc::new(AdmissionBook::new(
            1,
            Duration::from_secs(10),
            Duration::from_secs(7200),
        ));
        assert!(is_granted(&book.request(1, "a", 1)));
        let poll = book.wait(2, "b", 1, Duration::from_secs(30)).await;
        assert!(matches!(poll, Poll::GaveUp));
        assert!(
            book.snapshot().waiters.is_empty(),
            "a given-up waiter leaves the queue"
        );
        let queued = book.queued_time(2);
        assert!(
            queued >= Duration::from_secs(10) && queued < Duration::from_secs(12),
            "the whole wait is the run's queued time, got {queued:?}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn verify_takes_ignore_run_waiters_and_demand_holds_runs_back() {
        let book = book(2);
        assert!(is_granted(&book.request(1, "a", 1)));
        assert!(is_waiting(&book.request(2, "b", 2)));
        // The run waiter at the head wants two units; a verify unit that fits is not behind it.
        let grant = book.try_take_verify(1).expect("one unit is free");
        assert_eq!(book.free(), 0);
        let snapshot = book.snapshot();
        assert_eq!(snapshot.used, 2);
        assert!(
            snapshot
                .holders
                .iter()
                .any(|holder| holder.run_id.is_none())
        );
        drop(grant);
        assert_eq!(
            book.free(),
            1,
            "dropping a verify grant gives its weight back"
        );

        // With a verify unit waiting for capacity, runs are held back even though there is room.
        let book = book_of(1);
        book.set_verify_demand(true);
        assert!(book.snapshot().verify_demand);
        assert!(
            is_waiting(&book.request(1, "a", 1)),
            "demand holds the head back"
        );
        assert_eq!(book.free(), 1);
        book.set_verify_demand(false);
        assert!(is_granted(&book.request(1, "a", 1)));
    }

    #[tokio::test(start_paused = true)]
    async fn stale_verify_demand_stops_holding_runs_back() {
        let book = book(1);
        book.set_verify_demand(true);
        assert!(is_waiting(&book.request(1, "a", 1)));
        advance(Duration::from_secs(91)).await;
        assert!(
            is_granted(&book.request(1, "a", 1)),
            "a demand nobody refreshed for 90 s must lapse"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn queued_time_counts_finished_and_ongoing_waits() {
        let book = book(1);
        assert!(is_granted(&book.request(1, "h", 1)));
        assert_eq!(book.queued_time(2), Duration::ZERO);
        assert!(is_waiting(&book.request(2, "b", 1)));
        advance(Duration::from_secs(5)).await;
        assert_eq!(book.queued_time(2).as_secs(), 5, "an ongoing wait counts");
        assert!(book.release(1, "h"));
        assert!(is_granted(&book.request(2, "b", 1)));
        advance(Duration::from_secs(20)).await;
        assert_eq!(
            book.queued_time(2).as_secs(),
            5,
            "a finished wait stops growing"
        );

        // A second wait of the same run adds to the finished one.
        assert!(is_waiting(&book.request(2, "c", 1)));
        advance(Duration::from_secs(4)).await;
        assert_eq!(book.queued_time(2).as_secs(), 9);
    }

    #[tokio::test(start_paused = true)]
    async fn the_wall_clock_is_extended_by_time_spent_waiting() {
        let book = book(1);
        assert!(is_granted(&book.request(1, "h", 1)));
        let run = async {
            let poll = book.wait(2, "x", 1, Duration::from_secs(60)).await;
            assert!(is_granted(&poll));
            sleep(Duration::from_secs(5)).await;
            "done"
        };
        let releaser = async {
            sleep(Duration::from_secs(8)).await;
            book.release(1, "h");
        };
        // 8 s queued + 5 s of work is 13 s of wall clock, inside a 10 s budget only because the
        // time spent waiting for admission is not charged to it.
        let (result, ()) = tokio::join!(
            with_wall_clock(Some(&*book), 2, Duration::from_secs(10), run),
            releaser
        );
        assert!(matches!(result, Ok("done")));
    }

    #[tokio::test(start_paused = true)]
    async fn the_wall_clock_fires_when_nothing_waited() {
        let book = book(1);
        let started = Instant::now();
        let result = with_wall_clock(
            Some(&*book),
            3,
            Duration::from_secs(10),
            sleep(Duration::from_secs(60)),
        )
        .await;
        assert!(matches!(result, Err(WallClockElapsed)));
        assert_eq!(started.elapsed().as_secs(), 10);

        // No book at all is the plain timeout.
        let result = with_wall_clock(
            None,
            3,
            Duration::from_secs(10),
            sleep(Duration::from_secs(60)),
        )
        .await;
        assert!(matches!(result, Err(WallClockElapsed)));
        let result = with_wall_clock(None, 3, Duration::from_secs(10), async { 7 }).await;
        assert!(matches!(result, Ok(7)));
        // A test that builds a private book never installs the process-wide one.
        assert!(installed().is_none());
    }

    #[test]
    fn the_admission_block_parses_with_defaults_and_refuses_a_wait_above_the_ceiling() {
        let defaults = AdmissionConfig::default();
        assert!(defaults.enabled);
        assert_eq!(defaults.capacity, None);
        assert_eq!(defaults.max_wait_seconds, 900);
        assert_eq!(defaults.heavy_weight, 1);
        assert_eq!(defaults.full_suite_weight, 2);
        assert_eq!(defaults.grant_ttl_seconds, 7200);
        assert_eq!(parse_verify_config("").unwrap().admission, defaults);
        assert_eq!(
            parse_verify_config("capacity: 2\n").unwrap().admission,
            defaults,
            "a file without the block leaves it at its defaults"
        );

        let config = parse_verify_config("admission:\n  capacity: 3\n  enabled: false\n").unwrap();
        assert_eq!(config.admission.capacity, Some(3));
        assert!(!config.admission.enabled);
        assert_eq!(config.admission.max_wait_seconds, 900);

        assert_eq!(ADMISSION_MAX_WAIT_CEILING_SECONDS, 3000);
        let at_ceiling =
            format!("admission:\n  max_wait_seconds: {ADMISSION_MAX_WAIT_CEILING_SECONDS}\n");
        assert!(parse_verify_config(&at_ceiling).is_ok());
        let above = format!(
            "admission:\n  max_wait_seconds: {}\n",
            ADMISSION_MAX_WAIT_CEILING_SECONDS + 1
        );
        assert!(parse_verify_config(&above).is_err());
        assert!(parse_verify_config("admission:\n  max_wait_seconds: 0\n").is_err());
        assert!(parse_verify_config("admission:\n  capacity: 0\n").is_err());
        assert!(parse_verify_config("admission:\n  heavy_weight: 0\n").is_err());
        assert!(parse_verify_config("admission:\n  full_suite_weight: 0\n").is_err());
        assert!(parse_verify_config("admission:\n  grant_ttl_seconds: 59\n").is_err());
        assert!(parse_verify_config("admission:\n  grant_ttl_seconds: 60\n").is_ok());
        assert!(parse_verify_config("admission:\n  no_such_key: 1\n").is_err());
    }

    #[test]
    fn default_capacity_is_at_least_one() {
        assert!(default_capacity() >= 1);
    }
}
