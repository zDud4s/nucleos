//! Independent maintenance loops started with the daemon.
//!
//! Startup decides which pillars are configured; this module only starts the loops that keep
//! their already-enabled state current.

use crate::state::AppState;
use crate::{conductor, integrations};

/// Prune cached web pages hourly, even when no later request reads them.
pub fn spawn_web_retention_loop(state: AppState) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(std::time::Duration::from_secs(3600));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            match integrations::web::prune(
                &state.pool,
                state.web.retain_pages_days,
                chrono::Utc::now(),
            )
            .await
            {
                Ok(0) => {}
                Ok(pruned) => tracing::info!(pruned, "web: pages past the retention window"),
                // Best-effort, like the activity feed: a failed sweep is a full disk later, not
                // a reason to take the daemon down now.
                Err(error) => tracing::warn!(%error, "web: retention sweep failed"),
            }
        }
    });
}

/// Observe external agent sessions once a minute and record any liveness transitions.
pub fn spawn_conductor_liveness_loop(state: AppState) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(std::time::Duration::from_secs(60));
        // A pass that overruns its slot starts the next one a full period later, not in a burst.
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            // The tick is the fallback: a run that ended or a decision that was answered wakes the
            // pass at once (`conductor::sweep::wake`), so the driver does not wait out the minute.
            tokio::select! {
                _ = ticker.tick() => {}
                () = conductor::sweep::woken() => {}
            }
            match conductor::sweep::daemon_observations(&state).await {
                Ok(observed) => {
                    if let Err(error) = conductor::sweep::tick(
                        &state,
                        &observed,
                        &conductor::liveness::Limits::default(),
                        chrono::Utc::now(),
                    )
                    .await
                    {
                        tracing::warn!(%error, "conductor liveness sweep failed");
                    }
                }
                Err(error) => {
                    tracing::warn!(%error, "could not collect conductor observations")
                }
            }
        }
    });
}
