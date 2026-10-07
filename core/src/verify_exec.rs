//! The daemon's verification executor: a persisted queue of verification units
//! (`verify_runs`), scheduled by [`verify_sched::pick`] under a machine-wide weight capacity,
//! each run with its project's warm state — spec
//! `.ai/specs/2026-10-05-selecao-de-testes-design.md` §4.6.
//!
//! [`Executor::submit`] queues (or joins) a unit and wakes the worker; [`run_executor`] is the
//! worker loop; [`Executor::start_ready`] is one scheduling pass. Weight and in-use warm dirs are
//! released by a drop guard, so every way a unit ends gives its capacity back.
#![cfg_attr(not(test), allow(dead_code))]

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use sqlx::SqlitePool;
use tokio::sync::Notify;

use crate::config::VerifyConfig;
use crate::gate::{self, ArgvOutcome};
use crate::tests_map::{self, MapState};
use crate::verify_runs::{self, Claimed, Request, State, Submitted};
use crate::verify_sched::{self, Candidate};
use crate::warm;

/// A project's warm-state ceiling when its `autopilot.yaml` sets none.
pub const DEFAULT_PROJECT_DISK_CAP_GB: u64 = 30;
const POLL: Duration = Duration::from_millis(500);
const SWEEP_EVERY: Duration = Duration::from_secs(30 * 60);
/// How often a unit looks again at a warm dir the sweep is removing.
const REMOVING_POLL: Duration = Duration::from_millis(100);
const GIB: u64 = 1024 * 1024 * 1024;

/// Weight of a unit: what the heavy-command broker charges for the same program.
/// `cargo` (any case, with or without `.exe`) weighs 2, everything else 1, never above `capacity`.
pub fn weight_of(argv: &[String], capacity: i64) -> i64 {
    let program = argv.first().map(String::as_str).unwrap_or("");
    let base = program.rsplit(['/', '\\']).next().unwrap_or(program);
    let lower = base.to_ascii_lowercase();
    let name = lower.strip_suffix(".exe").unwrap_or(lower.as_str());
    let weight = if name == "cargo" { 2 } else { 1 };
    weight.min(capacity.max(1))
}

/// The argv that actually runs: the machine's broker, then the unit.
fn final_argv(prefix: &[String], argv: &[String]) -> Vec<String> {
    prefix.iter().chain(argv.iter()).cloned().collect()
}

/// How a finished process maps onto a row's status and exit code.
fn status_of(outcome: &ArgvOutcome) -> (&'static str, Option<i64>) {
    if outcome.timed_out || outcome.error.is_some() {
        return (
            verify_runs::STATUS_ERRORED,
            outcome.exit_code.map(i64::from),
        );
    }
    match outcome.exit_code {
        Some(0) => (verify_runs::STATUS_PASSED, Some(0)),
        Some(code) => (verify_runs::STATUS_FAILED, Some(i64::from(code))),
        None => (verify_runs::STATUS_ERRORED, None),
    }
}

/// The tail recorded for an outcome: why it errored, if it did, then the output.
fn tail_of(outcome: &ArgvOutcome) -> String {
    let head = if outcome.timed_out {
        Some(format!("timed out after {}s", outcome.duration.as_secs()))
    } else {
        outcome.error.clone()
    };
    match head {
        Some(head) if outcome.tail.is_empty() => head,
        Some(head) => format!("{head}\n{}", outcome.tail),
        None => outcome.tail.clone(),
    }
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn seconds_to_ms(seconds: u64) -> i64 {
    i64::try_from(seconds.saturating_mul(1000)).unwrap_or(i64::MAX)
}

#[derive(Default)]
struct InFlight {
    /// Weight of every unit reserved or running.
    used: i64,
    /// Per project, the counter value of its last start (higher = more recent).
    last_start: HashMap<Option<String>, u64>,
    counter: u64,
    /// Warm worktree dirs in use, with how many units hold each.
    in_use: HashMap<PathBuf, usize>,
    /// Warm worktree dirs the sweep is removing right now; no unit may hold one.
    removing: HashSet<PathBuf>,
}

pub struct Executor {
    pub pool: SqlitePool,
    pub config: VerifyConfig,
    pub machine_root: Option<PathBuf>,
    pub wake: Arc<Notify>,
    in_flight: Mutex<InFlight>,
    /// A warm sweep is running; the next tick skips rather than overlap it.
    sweeping: AtomicBool,
}

/// Holds a unit's weight and warm dirs; gives them back on drop, whatever path ended the unit.
struct Reservation {
    executor: Arc<Executor>,
    weight: i64,
    dirs: Vec<PathBuf>,
}

impl Reservation {
    /// Holds `dir` for this unit, first waiting out a sweep that is removing it.
    async fn hold_dir(&mut self, dir: PathBuf) {
        loop {
            {
                let mut state = self.executor.lock();
                if !state.removing.contains(&dir) {
                    *state.in_use.entry(dir.clone()).or_insert(0) += 1;
                    break;
                }
            }
            tokio::time::sleep(REMOVING_POLL).await;
        }
        self.dirs.push(dir);
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        {
            let mut state = self.executor.lock();
            state.used -= self.weight;
            for dir in self.dirs.drain(..) {
                if let Some(count) = state.in_use.get_mut(&dir) {
                    *count -= 1;
                    if *count == 0 {
                        state.in_use.remove(&dir);
                    }
                }
            }
        }
        self.executor.wake.notify_one();
    }
}

impl Executor {
    pub fn new(pool: SqlitePool, config: VerifyConfig, machine_root: Option<PathBuf>) -> Arc<Self> {
        Arc::new(Self {
            pool,
            config,
            machine_root,
            wake: Arc::new(Notify::new()),
            in_flight: Mutex::new(InFlight::default()),
            sweeping: AtomicBool::new(false),
        })
    }

    /// Short critical sections only; never held across an `.await`.
    fn lock(&self) -> MutexGuard<'_, InFlight> {
        self.in_flight
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn capacity(&self) -> i64 {
        i64::from(self.config.capacity).max(1)
    }

    /// The sweep's guard: grants `dir` for removal unless a unit holds it, and keeps units off it
    /// until [`Self::release_removal`].
    fn try_reserve_removal(&self, dir: &Path) -> bool {
        let mut state = self.lock();
        if state.in_use.contains_key(dir) {
            return false;
        }
        state.removing.insert(dir.to_path_buf());
        true
    }

    fn release_removal(&self, dir: &Path) {
        self.lock().removing.remove(dir);
    }

    /// Queues (or joins) a unit and wakes the worker.
    pub async fn submit(&self, mut request: Request) -> sqlx::Result<Submitted> {
        let capacity = self.capacity();
        request.weight = if request.weight > 0 {
            request.weight.min(capacity)
        } else {
            weight_of(&request.argv, capacity)
        };
        if request.timeout_ms <= 0 {
            request.timeout_ms = seconds_to_ms(self.config.unit_timeout_seconds);
        }
        let submitted = verify_runs::enqueue(&self.pool, &request, now_ms()).await?;
        self.wake.notify_one();
        Ok(submitted)
    }

    pub async fn get(&self, id: i64) -> sqlx::Result<Option<State>> {
        verify_runs::get(&self.pool, id).await
    }

    /// Starts every unit that fits now. Returns how many started. (One scheduling pass.)
    pub async fn start_ready(self: &Arc<Self>) -> usize {
        let aging_ms = seconds_to_ms(self.config.aging_seconds);
        let mut started = 0;
        loop {
            let queued = match verify_runs::queued(&self.pool).await {
                Ok(queued) => queued,
                Err(error) => {
                    tracing::warn!(%error, "verify executor: cannot read the queue");
                    break;
                }
            };
            // Pick and reserve under one short lock, so two passes never overcommit.
            let picked = {
                let mut state = self.lock();
                let capacity = self.capacity();
                // A row queued under a larger capacity would never fit: charge it the whole
                // capacity instead. Picked, reserved and released with this same value.
                let charge = |weight: i64| weight.clamp(1, capacity);
                let candidates: Vec<Candidate> = queued
                    .iter()
                    .map(|q| Candidate {
                        id: q.id,
                        project: q.project_id.as_deref(),
                        priority: q.priority,
                        weight: charge(q.weight),
                        enqueued_ms: q.enqueued_ms,
                    })
                    .collect();
                let free = capacity - state.used;
                let chosen =
                    verify_sched::pick(&candidates, free, &state.last_start, now_ms(), aging_ms)
                        .and_then(|id| queued.iter().find(|q| q.id == id));
                chosen.map(|chosen| {
                    let weight = charge(chosen.weight);
                    state.used += weight;
                    (chosen.id, weight, chosen.project_id.clone())
                })
            };
            let Some((id, weight, project)) = picked else {
                break;
            };
            let reservation = Reservation {
                executor: Arc::clone(self),
                weight,
                dirs: Vec::new(),
            };
            let claimed = match verify_runs::claim(&self.pool, id).await {
                Ok(Some(claimed)) => claimed,
                // Taken by another pass: the reservation drops and the loop looks again.
                Ok(None) => continue,
                Err(error) => {
                    tracing::warn!(%error, id, "verify executor: cannot claim a unit");
                    break;
                }
            };
            {
                let mut state = self.lock();
                state.counter += 1;
                let counter = state.counter;
                state.last_start.insert(project, counter);
            }
            tokio::spawn(run_unit(Arc::clone(self), claimed, reservation));
            started += 1;
        }
        started
    }

    /// The env a unit's warm state contributes. Empty when there is no machine root, no project,
    /// no valid map or no `warm:`; a `prepare` error is logged and the unit runs without it.
    async fn warm_env(&self, claimed: &Claimed) -> Vec<(String, String)> {
        let (Some(root), Some(project)) = (self.machine_root.clone(), claimed.project_id.clone())
        else {
            return Vec::new();
        };
        let worktree = PathBuf::from(&claimed.worktree);
        let result = tokio::task::spawn_blocking(move || {
            let map = match tests_map::load(&worktree) {
                MapState::Valid(map) if !map.tests.warm.is_empty() => map,
                _ => return Ok(Vec::new()),
            };
            warm::prepare(&root, &project, &worktree, &map.tests.warm)
        })
        .await;
        match result {
            Ok(Ok(env)) => env
                .into_iter()
                .map(|(var, path)| (var, path.to_string_lossy().into_owned()))
                .collect(),
            Ok(Err(error)) => {
                tracing::warn!(
                    %error,
                    id = claimed.id,
                    "verify executor: warm prepare failed; running without it"
                );
                Vec::new()
            }
            Err(error) => {
                tracing::warn!(
                    %error,
                    id = claimed.id,
                    "verify executor: warm prepare panicked; running without it"
                );
                Vec::new()
            }
        }
    }
}

/// Runs one claimed unit to its terminal row. The reservation is released when it drops.
async fn run_unit(executor: Arc<Executor>, claimed: Claimed, mut reservation: Reservation) {
    if let (Some(root), Some(project)) = (&executor.machine_root, &claimed.project_id) {
        // Lexical, no filesystem access; an invalid project id has no warm state to hold.
        if let Ok(dir) = warm::worktree_dir(root, project, Path::new(&claimed.worktree)) {
            reservation.hold_dir(dir).await;
        }
    }
    let env = executor.warm_env(&claimed).await;
    let argv = final_argv(&executor.config.broker_prefix, &claimed.argv);
    let timeout_ms = if claimed.timeout_ms > 0 {
        u64::try_from(claimed.timeout_ms).unwrap_or(u64::MAX)
    } else {
        executor.config.unit_timeout_seconds.saturating_mul(1000)
    };
    let outcome = gate::run_argv(
        &argv,
        Path::new(&claimed.worktree),
        &env,
        Duration::from_millis(timeout_ms),
    )
    .await;
    let (status, exit_code) = status_of(&outcome);
    let tail = tail_of(&outcome);
    let duration_ms = i64::try_from(outcome.duration.as_millis()).unwrap_or(i64::MAX);
    if let Err(error) = verify_runs::finish(
        &executor.pool,
        claimed.id,
        status,
        exit_code,
        duration_ms,
        (!tail.is_empty()).then_some(tail.as_str()),
    )
    .await
    {
        tracing::warn!(
            %error,
            id = claimed.id,
            "verify executor: cannot record a finished unit"
        );
    }
    drop(reservation);
}

/// The caps the sweep enforces: the machine's, and each project's from its `autopilot.yaml`.
fn sweep_caps(root: &Path, config: &VerifyConfig) -> warm::Caps {
    let mut project_bytes = HashMap::new();
    if let Ok(entries) = std::fs::read_dir(root.join("warm")) {
        for entry in entries.flatten() {
            if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                continue;
            }
            let Some(project) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            let cap = crate::config::load_schedule_rules(Some(root), &project)
                .ok()
                .and_then(|rules| rules.verify_disk_cap_gb)
                .unwrap_or(DEFAULT_PROJECT_DISK_CAP_GB);
            project_bytes.insert(project, cap.saturating_mul(GIB));
        }
    }
    warm::Caps {
        machine_bytes: config.disk_cap_gb.saturating_mul(GIB),
        project_bytes,
        default_project_bytes: DEFAULT_PROJECT_DISK_CAP_GB.saturating_mul(GIB),
    }
}

/// Marks a sweep as running; clears the mark when dropped, a panic included.
struct Sweeping(Arc<Executor>);

impl Sweeping {
    /// `None` while another sweep is still running.
    fn start(executor: &Arc<Executor>) -> Option<Self> {
        executor
            .sweeping
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .ok()
            .map(|_| Self(Arc::clone(executor)))
    }
}

impl Drop for Sweeping {
    fn drop(&mut self) {
        self.0.sweeping.store(false, Ordering::Release);
    }
}

/// One warm-state sweep, off the async threads. `_running` is released when it ends.
async fn sweep_once(executor: Arc<Executor>, _running: Sweeping) {
    let Some(root) = executor.machine_root.clone() else {
        return;
    };
    let config = executor.config.clone();
    let guard = Arc::clone(&executor);
    let result = tokio::task::spawn_blocking(move || {
        let caps = sweep_caps(&root, &config);
        warm::sweep(
            &root,
            &caps,
            &|dir| guard.try_reserve_removal(dir),
            &|dir| guard.release_removal(dir),
        )
    })
    .await;
    match result {
        Ok(removed) if !removed.is_empty() => {
            tracing::info!(
                count = removed.len(),
                "verify executor: warm sweep removed dirs"
            );
        }
        Ok(_) => {}
        Err(error) => tracing::warn!(%error, "verify executor: warm sweep panicked"),
    }
}

/// The worker: schedules on every wake and every `POLL`, sweeps warm state every `SWEEP_EVERY`.
pub async fn run_executor(executor: Arc<Executor>) {
    let first_sweep = tokio::time::Instant::now() + SWEEP_EVERY;
    let mut sweep = tokio::time::interval_at(first_sweep, SWEEP_EVERY);
    sweep.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = executor.wake.notified() => {}
            _ = tokio::time::sleep(POLL) => {}
            _ = sweep.tick() => {
                // A sweep still walking from the last tick is left to finish.
                if let Some(running) = Sweeping::start(&executor) {
                    tokio::spawn(sweep_once(Arc::clone(&executor), running));
                }
            }
        }
        executor.start_ready().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

    async fn test_pool() -> SqlitePool {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(":memory:")
                    .create_if_missing(true),
            )
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        pool
    }

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_owned()).collect()
    }

    fn request(worktree: &Path, argv: &[&str], fingerprint: Option<&str>, weight: i64) -> Request {
        Request {
            project_id: Some("proj".to_owned()),
            worktree: worktree.to_string_lossy().into_owned(),
            scope: verify_runs::SCOPE_FULL.to_owned(),
            origin: verify_runs::ORIGIN_RUN.to_owned(),
            origin_id: Some(1),
            requested_by: "agent".to_owned(),
            group_name: Some("core".to_owned()),
            kind: Some("test".to_owned()),
            argv: strings(argv),
            fingerprint: fingerprint.map(str::to_owned),
            priority: verify_runs::PRIORITY_INTERACTIVE,
            weight,
            timeout_ms: 0,
        }
    }

    async fn executor(capacity: u32, root: &Path) -> Arc<Executor> {
        let config = VerifyConfig {
            capacity,
            ..VerifyConfig::default()
        };
        Executor::new(test_pool().await, config, Some(root.to_path_buf()))
    }

    async fn wait_terminal(executor: &Arc<Executor>, id: i64) -> State {
        tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                let state = executor.get(id).await.unwrap().unwrap();
                if state.status != verify_runs::STATUS_QUEUED
                    && state.status != verify_runs::STATUS_RUNNING
                {
                    return state;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .expect("the unit did not finish in time")
    }

    fn outcome(exit_code: Option<i32>, timed_out: bool, error: Option<&str>) -> ArgvOutcome {
        ArgvOutcome {
            exit_code,
            tail: "out".to_owned(),
            duration: Duration::from_secs(3),
            timed_out,
            error: error.map(str::to_owned),
        }
    }

    #[tokio::test]
    async fn a_submitted_unit_runs_and_passes() {
        let root = tempfile::tempdir().unwrap();
        let wt = tempfile::tempdir().unwrap();
        let ex = executor(4, root.path()).await;
        let submitted = ex
            .submit(request(wt.path(), &["git", "--version"], None, 0))
            .await
            .unwrap();
        assert!(!submitted.joined);
        assert_eq!(ex.start_ready().await, 1);
        let state = wait_terminal(&ex, submitted.id).await;
        assert_eq!(state.status, verify_runs::STATUS_PASSED);
        assert_eq!(state.exit_code, Some(0));
    }

    #[tokio::test]
    async fn a_failing_unit_is_recorded_as_failed() {
        let root = tempfile::tempdir().unwrap();
        let wt = tempfile::tempdir().unwrap();
        let ex = executor(4, root.path()).await;
        let argv = ["git", "definitely-not-a-git-subcommand"];
        let submitted = ex.submit(request(wt.path(), &argv, None, 0)).await.unwrap();
        assert_eq!(ex.start_ready().await, 1);
        let state = wait_terminal(&ex, submitted.id).await;
        assert_eq!(state.status, verify_runs::STATUS_FAILED);
        assert!(matches!(state.exit_code, Some(code) if code != 0));
    }

    #[test]
    fn status_of_maps_every_outcome() {
        let timed = outcome(None, true, None);
        assert_eq!(status_of(&timed).0, verify_runs::STATUS_ERRORED);
        assert!(tail_of(&timed).starts_with("timed out after 3s"));

        let spawn_failed = outcome(None, false, Some("spawn failed"));
        assert_eq!(status_of(&spawn_failed).0, verify_runs::STATUS_ERRORED);
        assert!(tail_of(&spawn_failed).starts_with("spawn failed"));

        assert_eq!(
            status_of(&outcome(Some(0), false, None)),
            (verify_runs::STATUS_PASSED, Some(0))
        );
        assert_eq!(
            status_of(&outcome(Some(2), false, None)),
            (verify_runs::STATUS_FAILED, Some(2))
        );
        assert_eq!(
            status_of(&outcome(None, false, None)),
            (verify_runs::STATUS_ERRORED, None)
        );
        assert_eq!(tail_of(&outcome(Some(0), false, None)), "out");
    }

    #[tokio::test]
    async fn two_equal_submits_run_once() {
        let root = tempfile::tempdir().unwrap();
        let wt = tempfile::tempdir().unwrap();
        let ex = executor(4, root.path()).await;
        let argv = ["git", "--version"];
        let first = ex
            .submit(request(wt.path(), &argv, Some("fp"), 0))
            .await
            .unwrap();
        let second = ex
            .submit(request(wt.path(), &argv, Some("fp"), 0))
            .await
            .unwrap();
        assert_eq!(first.id, second.id);
        assert!(second.joined);
        let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM verify_runs")
            .fetch_one(&ex.pool)
            .await
            .unwrap();
        assert_eq!(rows, 1);
    }

    /// Parks every unit of `wt` before it runs, its weight held, until `release_removal`: the
    /// unit waits on a warm dir the sweep is "removing".
    fn park(ex: &Arc<Executor>, root: &Path, wt: &Path) -> PathBuf {
        let dir = warm::worktree_dir(root, "proj", wt).unwrap();
        assert!(ex.try_reserve_removal(&dir));
        dir
    }

    #[tokio::test]
    async fn capacity_holds_back_what_does_not_fit() {
        let root = tempfile::tempdir().unwrap();
        let wt = tempfile::tempdir().unwrap();
        let ex = executor(2, root.path()).await;
        let parked = park(&ex, root.path(), wt.path());
        let argv = ["git", "--version"];
        let first = ex.submit(request(wt.path(), &argv, None, 2)).await.unwrap();
        let second = ex.submit(request(wt.path(), &argv, None, 2)).await.unwrap();
        // The first cannot finish (and give its weight back) while parked, so this is exact.
        assert_eq!(ex.start_ready().await, 1);
        assert_eq!(ex.lock().used, 2);
        let waiting = ex.get(second.id).await.unwrap().unwrap();
        assert_eq!(waiting.status, verify_runs::STATUS_QUEUED);
        ex.release_removal(&parked);
        wait_terminal(&ex, first.id).await;
    }

    #[tokio::test]
    async fn a_row_heavier_than_the_capacity_still_starts() {
        let root = tempfile::tempdir().unwrap();
        let wt = tempfile::tempdir().unwrap();
        let ex = executor(2, root.path()).await;
        let parked = park(&ex, root.path(), wt.path());
        // Queued straight to the table, as if under an older, larger capacity.
        let row = request(wt.path(), &["git", "--version"], None, 4);
        let queued = verify_runs::enqueue(&ex.pool, &row, now_ms())
            .await
            .unwrap();
        assert_eq!(ex.start_ready().await, 1);
        assert_eq!(ex.lock().used, 2);
        ex.release_removal(&parked);
        wait_terminal(&ex, queued.id).await;
    }

    #[tokio::test]
    async fn a_unit_waits_while_its_warm_dir_is_being_removed() {
        let root = tempfile::tempdir().unwrap();
        let wt = tempfile::tempdir().unwrap();
        let ex = executor(4, root.path()).await;
        let parked = park(&ex, root.path(), wt.path());
        let submitted = ex
            .submit(request(wt.path(), &["git", "--version"], None, 0))
            .await
            .unwrap();
        assert_eq!(ex.start_ready().await, 1);
        tokio::time::sleep(Duration::from_millis(300)).await;
        let state = ex.get(submitted.id).await.unwrap().unwrap();
        assert_eq!(state.status, verify_runs::STATUS_RUNNING);
        assert!(!ex.lock().in_use.contains_key(&parked));
        ex.release_removal(&parked);
        let state = wait_terminal(&ex, submitted.id).await;
        assert_eq!(state.status, verify_runs::STATUS_PASSED);
    }

    #[tokio::test]
    async fn a_dir_in_use_cannot_be_reserved_for_removal() {
        let root = tempfile::tempdir().unwrap();
        let ex = executor(4, root.path()).await;
        let dir = root.path().join("warm/proj/abc");
        ex.lock().in_use.insert(dir.clone(), 1);
        assert!(!ex.try_reserve_removal(&dir));
        assert!(!ex.lock().removing.contains(&dir));
        ex.lock().in_use.clear();
        assert!(ex.try_reserve_removal(&dir));
        ex.release_removal(&dir);
        assert!(ex.lock().removing.is_empty());
    }

    #[tokio::test]
    async fn a_sweep_does_not_overlap_another() {
        let root = tempfile::tempdir().unwrap();
        let ex = executor(4, root.path()).await;
        let running = Sweeping::start(&ex).expect("the first sweep starts");
        assert!(Sweeping::start(&ex).is_none());
        drop(running);
        assert!(Sweeping::start(&ex).is_some());
    }

    #[test]
    fn weight_of_charges_cargo_double() {
        assert_eq!(weight_of(&strings(&["cargo", "test"]), 4), 2);
        assert_eq!(weight_of(&strings(&["C:\\x\\cargo.exe", "test"]), 4), 2);
        assert_eq!(weight_of(&strings(&["CARGO.EXE"]), 4), 2);
        assert_eq!(weight_of(&strings(&["git", "status"]), 4), 1);
        assert_eq!(weight_of(&strings(&["cargo"]), 1), 1);
    }

    #[tokio::test]
    async fn a_unit_gets_its_warm_env() {
        let root = tempfile::tempdir().unwrap();
        let wt = tempfile::tempdir().unwrap();
        let map = concat!(
            "version: 1\n",
            "tests:\n",
            "  groups:\n",
            "    core:\n",
            "      paths: [core/]\n",
            "      command: cargo test -p nucleos-core\n",
            "  warm:\n",
            "    NUCLEOS_PROBE_DIR:\n",
            "      dir: probe\n",
        );
        std::fs::write(wt.path().join(tests_map::MAP_FILE), map).unwrap();
        let ex = executor(4, root.path()).await;
        let claimed = Claimed {
            id: 0,
            project_id: Some("proj".to_owned()),
            worktree: wt.path().to_string_lossy().into_owned(),
            argv: strings(&["git", "--version"]),
            weight: 1,
            timeout_ms: 1000,
        };
        let env = ex.warm_env(&claimed).await;
        let expected = warm::worktree_dir(root.path(), "proj", wt.path())
            .unwrap()
            .join("probe");
        let value = env
            .iter()
            .find(|(var, _)| var == "NUCLEOS_PROBE_DIR")
            .map(|(_, value)| PathBuf::from(value))
            .expect("NUCLEOS_PROBE_DIR is in the env");
        assert_eq!(value, expected);
        assert!(expected.is_dir());
    }

    #[test]
    fn the_broker_prefix_goes_in_front() {
        let prefix = strings(&["python", "heavy.py", "--"]);
        assert_eq!(
            final_argv(&prefix, &strings(&["cargo", "test"])),
            strings(&["python", "heavy.py", "--", "cargo", "test"])
        );
        assert_eq!(final_argv(&[], &strings(&["git"])), strings(&["git"]));
    }
}
