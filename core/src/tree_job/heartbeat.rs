//! The tree watchdog's real heartbeat (spec B5): what renews a node's lease (`watchdog::beat`).
//!
//! A lease of 30 minutes only works if something renews it while the node is alive:
//!
//! - the run's own stdout events (a [`Watch`] reads the live transcript of the run);
//! - an open tool call (a `tool_use` with no `tool_result` yet: the stream is quiet because the
//!   tool is working, as the runner's own silence deadline reads it);
//! - every PreToolUse hook call, and every wait inside a hook request (`/hooks/ask-wait`, the
//!   admission wait of `/hooks/admit`), which [`while_waiting`] keeps beating for as long as it lasts.
//!
//! Every path ends in [`beat_run`], which writes at most once per run every [`BEAT_EVERY`]: a busy
//! run calls it on every tool call, and the lease needs one write in minutes, not one per call.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use sqlx::SqlitePool;

use super::watchdog;

/// The most often a run's node is written to.
pub const BEAT_EVERY: Duration = Duration::from_secs(5 * 60);
/// How often a [`Watch`] looks at the transcript and a wait looks at the clock.
const POLL: Duration = Duration::from_secs(30);
/// Entries of the throttle past which the stale ones are dropped.
const THROTTLE_PRUNE_AT: usize = 256;

/// Remembers when each run last passed, so [`BEAT_EVERY`] is kept without touching the database.
#[derive(Default)]
struct Throttle {
    last: Mutex<HashMap<i64, Instant>>,
}

impl Throttle {
    /// Whether a write for `run_id` is due at `now`; when it is, the moment is remembered.
    fn admit(&self, run_id: i64, now: Instant) -> bool {
        let mut last = self
            .last
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let fresh = |at: &Instant| now.saturating_duration_since(*at) < BEAT_EVERY;
        if last.get(&run_id).is_some_and(fresh) {
            return false;
        }
        if last.len() >= THROTTLE_PRUNE_AT {
            last.retain(|_, at| fresh(at));
        }
        last.insert(run_id, now);
        true
    }
}

static THROTTLE: LazyLock<Throttle> = LazyLock::new(Throttle::default);

/// Marks the tree node running `run_id` alive, at most once per [`BEAT_EVERY`]. A run that is no
/// node's (most are not) costs one read per period. Best effort: a lease that cannot be renewed is
/// logged, never an error of the call that asked.
pub async fn beat_run(pool: &SqlitePool, run_id: i64) {
    beat_run_at(pool, &THROTTLE, run_id, Instant::now()).await;
}

async fn beat_run_at(pool: &SqlitePool, throttle: &Throttle, run_id: i64, now: Instant) {
    if !throttle.admit(run_id, now) {
        return;
    }
    let node: Result<Option<String>, _> =
        sqlx::query_scalar("SELECT id FROM work_nodes WHERE run_id = ? AND status = 'running'")
            .bind(run_id.to_string())
            .fetch_optional(pool)
            .await;
    match node {
        Ok(Some(node_id)) => {
            if let Err(error) = watchdog::beat(pool, &node_id).await {
                tracing::warn!(node = %node_id, run_id, %error, "heartbeat could not renew the lease");
            }
        }
        Ok(None) => {}
        Err(error) => tracing::warn!(run_id, %error, "heartbeat could not look up the run's node"),
    }
}

/// Drives `wait` to its end, renewing the lease of `run_id` every [`POLL`] meanwhile: a run parked
/// in a hook request is waiting, not lost. Beats once at the start.
pub async fn while_waiting<F: Future>(pool: &SqlitePool, run_id: i64, wait: F) -> F::Output {
    tokio::pin!(wait);
    let mut tick = tokio::time::interval(POLL);
    loop {
        tokio::select! {
            out = &mut wait => return out,
            _ = tick.tick() => beat_run(pool, run_id).await,
        }
    }
}

/// What the live transcript of a run says about it, read incrementally.
#[derive(Default)]
struct Pulse {
    /// Bytes of the transcript already seen.
    len: usize,
    /// Bytes already read as complete lines.
    parsed: usize,
    /// Tool calls started and not answered.
    open_tools: HashSet<String>,
}

impl Pulse {
    /// Reads what the transcript gained since the last look. Alive when it gained anything, or a
    /// tool call is still open.
    fn observe(&mut self, transcript: &str) -> bool {
        let grew = transcript.len() > self.len;
        self.len = transcript.len();
        let fresh = transcript.get(self.parsed..).unwrap_or("");
        // Only complete lines: the last one may still be arriving.
        if let Some(end) = fresh.rfind('\n') {
            for line in fresh[..end].lines() {
                if line.contains("\"type\":\"result\"") {
                    // A turn that ended has no tool call left running, as the runner reads it too.
                    self.open_tools.clear();
                }
                crate::execution::runner::track_open_tools(line, &mut self.open_tools);
            }
            self.parsed += end + 1;
        }
        grew || !self.open_tools.is_empty()
    }
}

/// Renews the lease of a run from its own stdout events until dropped.
pub struct Watch(tokio::task::AbortHandle);

impl Drop for Watch {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Starts watching the transcript the runner mirrors a live run's stdout into. One per attempt,
/// held for the attempt, like the context-fill mirror beside it.
pub fn watch_run(pool: &SqlitePool, run_id: i64, transcript: Arc<Mutex<String>>) -> Watch {
    let pool = pool.clone();
    Watch(
        tokio::spawn(async move {
            let mut pulse = Pulse::default();
            loop {
                tokio::time::sleep(POLL).await;
                pulse_once(&pool, run_id, &mut pulse, &transcript).await;
            }
        })
        .abort_handle(),
    )
}

/// One look at the transcript: a sign of life renews the lease.
async fn pulse_once(pool: &SqlitePool, run_id: i64, pulse: &mut Pulse, transcript: &Mutex<String>) {
    let alive = match transcript.lock() {
        Ok(text) => pulse.observe(&text),
        Err(_) => false,
    };
    if alive {
        beat_run(pool, run_id).await;
    }
}

#[cfg(test)]
mod tests {

    use super::*;
    use crate::state::AppState;

    const OLD: &str = "2026-10-31T00:00:00+00:00";

    async fn state() -> AppState {
        crate::execution::runs::testkit::test_state_with_runner(
            Some(Duration::from_secs(60)),
            Duration::from_secs(600),
        )
        .await
        .0
    }

    /// A running node with run `run_id`.
    async fn seed_node(state: &AppState, node_id: &str, run_id: i64) {
        sqlx::query(
            "INSERT INTO work_nodes (id, root_id, depth, kind, title, goal, status, run_id, \
                                     handoff_path, created_at, updated_at) \
             VALUES (?, 'hb-root', 0, 'orchestrator', 't', 'g', 'running', ?, 'C:/h.md', ?, ?)",
        )
        .bind(node_id)
        .bind(run_id.to_string())
        .bind(OLD)
        .bind(OLD)
        .execute(&state.pool)
        .await
        .unwrap();
    }

    async fn beat_at(state: &AppState, node_id: &str) -> Option<String> {
        sqlx::query_scalar("SELECT beat_at FROM node_watchdog WHERE node_id = ?")
            .bind(node_id)
            .fetch_optional(&state.pool)
            .await
            .unwrap()
            .flatten()
    }

    /// Puts the lease back to long ago, so the next write is visible as a change.
    async fn age_lease(state: &AppState, node_id: &str) {
        sqlx::query("UPDATE node_watchdog SET beat_at = ? WHERE node_id = ?")
            .bind(OLD)
            .bind(node_id)
            .execute(&state.pool)
            .await
            .unwrap();
    }

    fn tool_use(id: &str) -> String {
        format!(
            "{{\"type\":\"assistant\",\"message\":{{\"content\":[{{\"type\":\"tool_use\",\"id\":\"{id}\"}}]}}}}\n"
        )
    }

    fn tool_result(id: &str) -> String {
        format!(
            "{{\"type\":\"user\",\"message\":{{\"content\":[{{\"type\":\"tool_result\",\"tool_use_id\":\"{id}\"}}]}}}}\n"
        )
    }

    #[test]
    fn the_throttle_lets_one_write_per_run_through_every_five_minutes() {
        let throttle = Throttle::default();
        let start = Instant::now();
        assert!(throttle.admit(1, start));
        assert!(!throttle.admit(1, start + Duration::from_secs(60)));
        assert!(!throttle.admit(1, start + BEAT_EVERY - Duration::from_secs(1)));
        assert!(throttle.admit(1, start + BEAT_EVERY));
        assert!(!throttle.admit(1, start + BEAT_EVERY + Duration::from_secs(1)));
        assert!(
            throttle.admit(2, start + Duration::from_secs(60)),
            "another run"
        );
    }

    #[tokio::test]
    async fn a_second_beat_inside_the_window_writes_nothing_and_one_after_it_does() {
        let state = state().await;
        seed_node(&state, "hb-throttle", 7101).await;
        let throttle = Throttle::default();
        let start = Instant::now();

        beat_run_at(&state.pool, &throttle, 7101, start).await;
        assert!(beat_at(&state, "hb-throttle").await.is_some());
        age_lease(&state, "hb-throttle").await;

        beat_run_at(
            &state.pool,
            &throttle,
            7101,
            start + Duration::from_secs(120),
        )
        .await;
        assert_eq!(beat_at(&state, "hb-throttle").await.as_deref(), Some(OLD));

        beat_run_at(&state.pool, &throttle, 7101, start + BEAT_EVERY).await;
        assert_ne!(beat_at(&state, "hb-throttle").await.as_deref(), Some(OLD));
    }

    #[tokio::test]
    async fn a_run_that_is_no_nodes_writes_nothing() {
        let state = state().await;
        beat_run_at(&state.pool, &Throttle::default(), 7102, Instant::now()).await;
        let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM node_watchdog")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(rows, 0);
    }

    #[test]
    fn a_pulse_is_alive_when_the_transcript_grows_and_not_when_it_stands_still() {
        let mut pulse = Pulse::default();
        assert!(!pulse.observe(""));
        assert!(pulse.observe("{\"type\":\"system\"}\n"));
        assert!(!pulse.observe("{\"type\":\"system\"}\n"));
        assert!(pulse.observe("{\"type\":\"system\"}\n{\"type\":\"assistant\"}\n"));
    }

    #[test]
    fn an_open_tool_keeps_a_quiet_transcript_alive_until_its_result_or_the_turn_ends() {
        let mut pulse = Pulse::default();
        let mut text = tool_use("toolu_1");
        assert!(pulse.observe(&text));
        assert!(
            pulse.observe(&text),
            "no new line, but the tool is still working"
        );

        text.push_str(&tool_result("toolu_1"));
        assert!(pulse.observe(&text), "the result is itself a line");
        assert!(
            !pulse.observe(&text),
            "answered: nothing is open and nothing moves"
        );

        text.push_str(&tool_use("toolu_2"));
        assert!(pulse.observe(&text));
        text.push_str("{\"type\":\"result\"}\n");
        assert!(pulse.observe(&text));
        assert!(
            !pulse.observe(&text),
            "a turn that ended leaves no tool open"
        );
    }

    #[test]
    fn a_line_still_arriving_is_read_once_it_is_complete() {
        let mut pulse = Pulse::default();
        let line = tool_use("toolu_9");
        let (half, rest) = line.split_at(20);
        assert!(pulse.observe(half));
        assert!(pulse.open_tools.is_empty());
        let whole = format!("{half}{rest}");
        assert!(pulse.observe(&whole));
        assert_eq!(pulse.open_tools.len(), 1);
    }

    #[tokio::test]
    async fn a_run_event_renews_the_nodes_lease() {
        let state = state().await;
        seed_node(&state, "hb-events", 7103).await;
        let transcript = Mutex::new(String::new());
        let mut pulse = Pulse::default();

        pulse_once(&state.pool, 7103, &mut pulse, &transcript).await;
        assert!(
            beat_at(&state, "hb-events").await.is_none(),
            "silence is no sign of life"
        );

        transcript
            .lock()
            .unwrap()
            .push_str("{\"type\":\"assistant\"}\n");
        pulse_once(&state.pool, 7103, &mut pulse, &transcript).await;
        assert!(beat_at(&state, "hb-events").await.is_some());
    }

    #[tokio::test]
    async fn an_open_tool_renews_the_lease_with_no_event_at_all() {
        let state = state().await;
        seed_node(&state, "hb-tool", 7104).await;
        let transcript = Mutex::new(tool_use("toolu_1"));
        let mut pulse = Pulse::default();
        pulse_once(&state.pool, 7104, &mut pulse, &transcript).await;
        age_lease(&state, "hb-tool").await;

        // Still quiet, still open; the next throttle window is simulated by a fresh run id key.
        THROTTLE.last.lock().unwrap().remove(&7104);
        pulse_once(&state.pool, 7104, &mut pulse, &transcript).await;

        assert_ne!(beat_at(&state, "hb-tool").await.as_deref(), Some(OLD));
    }

    #[tokio::test]
    async fn a_wait_beats_at_its_start_and_returns_what_it_waited_for() {
        let state = state().await;
        seed_node(&state, "hb-wait", 7106).await;

        let out = while_waiting(&state.pool, 7106, async { 42 }).await;

        assert_eq!(out, 42);
        // The first tick of the interval is immediate, but `select!` may take the ready wait first:
        // the beat is only guaranteed once the wait outlasts a tick.
        let out = while_waiting(&state.pool, 7106, async {
            tokio::time::sleep(Duration::from_millis(50)).await;
            7
        })
        .await;
        assert_eq!(out, 7);
        assert!(beat_at(&state, "hb-wait").await.is_some());
    }
}
