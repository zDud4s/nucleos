//! The per-ticket done watcher: the verdict of a verification is pushed, not polled for.
//!
//! `post_verify` holds a caller only `verify::HOLD`; a ticket asked for by a run that the hold left
//! unfinished is then watched here until it is done, and the run is handed its verdict through
//! `verify_wake::deliver`. A ticket that finished within the hold, or one asked for by anyone but a
//! run, gets no watcher: the caller already holds the verdict (or reads it through the CLI).

use std::time::{Duration, Instant};

use crate::execution::runs::verify_wake;
use crate::state::AppState;
use crate::verify::wait_ticket;

/// How often the watcher re-reads a ticket that is not done yet.
const WATCH_POLL: Duration = Duration::from_secs(1);

/// After this long the watcher gives up on a ticket: the executor's own unit timeouts end a run
/// long before, so a ticket still open here is one nobody will finish. `verify_wake::rearm` reads
/// it too, so a restart does not give a wake another full span.
pub(crate) const WATCH_LIMIT: Duration = Duration::from_secs(3 * 60 * 60);

/// How long the watcher waits after seeing a ticket done before it delivers. A run that was holding
/// `/verify/status` for the same ticket gets the verdict in that response and records `seen_at`
/// just after; this lets that write land first, so the run is not told twice.
const SEEN_GRACE: Duration = Duration::from_secs(2);

/// Watches ticket `id` in its own task and, once it is done, delivers it to run `run_id`.
pub fn spawn_watch(state: AppState, id: i64, run_id: i64) {
    spawn_watch_for(state, id, run_id, WATCH_LIMIT);
}

/// `spawn_watch` with its own span: `verify_wake::rearm` watches a ticket the restart requeued for
/// less than a full `WATCH_LIMIT`.
pub fn spawn_watch_for(state: AppState, id: i64, run_id: i64, limit: Duration) {
    tokio::spawn(async move { watch(&state, id, run_id, limit).await });
}

async fn watch(state: &AppState, id: i64, run_id: i64, limit: Duration) {
    let started = Instant::now();
    loop {
        match wait_ticket(&state.pool, id, WATCH_POLL).await {
            Ok(Some((ticket, _))) if ticket.done => {
                tokio::time::sleep(SEEN_GRACE).await;
                // `deliver` skips a ticket the run has read in the meantime.
                verify_wake::deliver(state, run_id, id, &ticket).await;
                return;
            }
            Ok(Some(_)) => {}
            Ok(None) => {
                verify_wake::abandon(state, run_id, id, "its ticket no longer exists").await;
                return;
            }
            Err(error) => {
                tracing::warn!(%error, id, "verify watcher: cannot read the ticket");
                tokio::time::sleep(WATCH_POLL).await;
            }
        }
        if started.elapsed() >= limit {
            tracing::warn!(
                id,
                "verify watcher: gave up on a ticket that never finished"
            );
            verify_wake::abandon(
                state,
                run_id,
                id,
                "its verification never finished (the daemon stopped waiting for it)",
            )
            .await;
            return;
        }
    }
}
