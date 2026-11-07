//! What the liveness sweep may ask the OS about a run: which process its CLI is, and when the
//! current attempt began.
//!
//! The sweep used to read "this run has no entry in `run_handles`" as "its process is gone". The two
//! are different facts: the handle is the daemon's bookkeeping for the driver TASK, the CLI is a child
//! process, and a run whose handle was released while its CLI kept working was cancelled mid-work
//! (2026-11-06, runs 365, 387, 419, 457). It also measured silence from the run's `created_at`, so a
//! retry attempt (a fresh CLI, a fresh and empty transcript) inherited the age of the attempts before it.
//!
//! Process-global rather than a field of `AppState` because the runner is spawned without one: it
//! learns which run it serves only from the `NUCLEOS_RUN_ID` the daemon put in the request's env.

use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

#[derive(Clone, Copy)]
struct Entry {
    attempt_started: DateTime<Utc>,
    pid: Option<u32>,
    /// When the post-run gate began, while it runs. The CLI is dead by then and the transcript tail
    /// is silent, so this is the only sign of life the run gives.
    gate_started: Option<DateTime<Utc>>,
}

static ATTEMPTS: LazyLock<Mutex<HashMap<i64, Entry>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn attempts() -> std::sync::MutexGuard<'static, HashMap<i64, Entry>> {
    ATTEMPTS.lock().unwrap_or_else(|e| e.into_inner())
}

/// A new attempt of `run_id` starts now, with no CLI yet. Called where the attempt's transcript
/// buffer is published.
pub fn begin_attempt(run_id: i64) {
    attempts().insert(
        run_id,
        Entry {
            attempt_started: Utc::now(),
            pid: None,
            gate_started: None,
        },
    );
}

/// The run is over (or its task is gone): nothing is left to ask about.
pub fn forget(run_id: i64) {
    attempts().remove(&run_id);
}

/// When the current attempt of `run_id` began, if it is known.
pub fn attempt_started(run_id: i64) -> Option<DateTime<Utc>> {
    attempts().get(&run_id).map(|entry| entry.attempt_started)
}

/// The pid of `run_id`'s CLI, while it is running.
pub fn pid_of(run_id: i64) -> Option<u32> {
    attempts().get(&run_id).and_then(|entry| entry.pid)
}

/// Slack past `DEFAULT_GATE_TIMEOUT` before a gate marker stops protecting its run: the gate may
/// queue on heavy admission and its final write comes after the command returns.
pub const GATE_MARGIN: chrono::Duration = chrono::Duration::minutes(5);

/// The latest moment a gate that began at `started` can still be legitimately running.
fn gate_deadline(started: DateTime<Utc>) -> DateTime<Utc> {
    started
        + chrono::Duration::from_std(crate::state::DEFAULT_GATE_TIMEOUT).unwrap_or_default()
        + GATE_MARGIN
}

/// Until when `run_id`'s post-run gate is a legitimate reason for a quiet transcript, if one is in
/// flight. Bounded: a marker that outlives the gate's own timeout stops protecting the run.
pub fn gate_in_flight_until(run_id: i64) -> Option<DateTime<Utc>> {
    attempts()
        .get(&run_id)
        .and_then(|entry| entry.gate_started)
        .map(gate_deadline)
}

/// Marks the post-run gate of `run_id` as running, until the guard drops (on every exit path,
/// including the task being aborted).
pub fn begin_gate(run_id: i64) -> GateInFlight {
    set_gate_started(run_id, Utc::now())
}

fn set_gate_started(run_id: i64, at: DateTime<Utc>) -> GateInFlight {
    if let Some(entry) = attempts().get_mut(&run_id) {
        entry.gate_started = Some(at);
    }
    GateInFlight { run_id }
}

/// Test seam: a gate that began at `at`.
#[cfg(test)]
pub fn begin_gate_at(run_id: i64, at: DateTime<Utc>) -> GateInFlight {
    set_gate_started(run_id, at)
}

pub struct GateInFlight {
    run_id: i64,
}

impl Drop for GateInFlight {
    fn drop(&mut self) {
        if let Some(entry) = attempts().get_mut(&self.run_id) {
            entry.gate_started = None;
        }
    }
}

/// Clears the pid when the CLI's attempt ends, and only if it is still this one's.
pub struct CliPid {
    run_id: i64,
    pid: u32,
}

impl Drop for CliPid {
    fn drop(&mut self) {
        if let Some(entry) = attempts().get_mut(&self.run_id)
            && entry.pid == Some(self.pid)
        {
            entry.pid = None;
        }
    }
}

/// Records `pid` as the CLI of the run `env` names. `None` for a request that names no run (a
/// conversation or a one-off call) or a spawn that gave no pid. Declare the guard AFTER the child so
/// it drops first, while the pid is still ours.
pub fn register(env: &[(String, String)], pid: Option<u32>) -> Option<CliPid> {
    let pid = pid?;
    let run_id = env
        .iter()
        .find(|(name, _)| name == "NUCLEOS_RUN_ID")?
        .1
        .parse::<i64>()
        .ok()?;
    // Only a run whose attempt was announced: an entry created here would have nobody to `forget`
    // it, and a conversation turn (which has a run id but no retry loop) would leak one.
    attempts().get_mut(&run_id)?.pid = Some(pid);
    Some(CliPid { run_id, pid })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(id: i64) -> Vec<(String, String)> {
        vec![("NUCLEOS_RUN_ID".to_string(), id.to_string())]
    }

    #[test]
    fn a_pid_lasts_exactly_as_long_as_its_guard() {
        begin_attempt(9100001);
        let guard = register(&env(9100001), Some(4242));
        assert_eq!(pid_of(9100001), Some(4242));
        drop(guard);
        assert_eq!(pid_of(9100001), None);
        assert!(attempt_started(9100001).is_some());
        forget(9100001);
        assert!(attempt_started(9100001).is_none());
    }

    #[test]
    fn a_gate_marker_lasts_as_long_as_its_guard_and_is_bounded() {
        begin_attempt(9100003);
        assert_eq!(gate_in_flight_until(9100003), None);
        let guard = begin_gate(9100003);
        let until = gate_in_flight_until(9100003).expect("marker set");
        assert!(until > Utc::now() + chrono::Duration::minutes(45));
        drop(guard);
        assert_eq!(gate_in_flight_until(9100003), None);
        forget(9100003);
    }

    #[test]
    fn a_stale_guard_does_not_clear_its_successors_pid() {
        begin_attempt(9100002);
        let first = register(&env(9100002), Some(1));
        let second = register(&env(9100002), Some(2));
        drop(first);
        assert_eq!(pid_of(9100002), Some(2));
        drop(second);
        assert_eq!(pid_of(9100002), None);
        forget(9100002);
    }

    #[test]
    fn a_request_naming_no_run_registers_nothing() {
        assert!(register(&[], Some(7)).is_none());
        assert!(register(&env(9100003), Some(7)).is_none());
    }
}
