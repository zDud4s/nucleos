//! `POST /system/restart`: restart the daemon once running work has settled.

use super::*;

#[derive(Deserialize)]
pub(super) struct SystemRestartQuery {
    /// How long to wait for running work, in seconds. Absent means the default; capped.
    timeout_secs: Option<u64>,
    /// The run asking (`NUCLEOS_RUN_ID`). Its chat, not just the run, is left out of the wait.
    requester_run: Option<i64>,
    /// Skip the wait: the runs still live are left to the next start's orphan reconciliation.
    force: Option<bool>,
}

/// `POST /system/restart`: restart the daemon once no run is working, without interrupting one.
///
/// Answers 202 at once with the readiness it saw; the wait happens in a task, exactly as in
/// `POST /system/shutdown`, and new work is refused from this moment on. When the wait ends the
/// daemon leaves by the graceful path and is started again (`relaunch.rs`): under `--supervise` by
/// the supervisor, otherwise by a relauncher it spawns. A timed-out wait keeps the daemon up,
/// spawns nothing and reopens the door, so a later restart can start. `force=true` does not wait.
pub(super) async fn post_system_restart(
    State(state): State<AppState>,
    Query(query): Query<SystemRestartQuery>,
) -> Result<(StatusCode, Json<crate::shutdown::Readiness>), StatusCode> {
    let timeout = query
        .timeout_secs
        .map(std::time::Duration::from_secs)
        .unwrap_or(crate::shutdown::DEFAULT_DRAIN_TIMEOUT)
        .min(crate::shutdown::MAX_DRAIN_TIMEOUT);
    let requester_run = query.requester_run;
    let force = query.force.unwrap_or(false);

    if crate::shutdown::begin_draining() {
        tracing::warn!(
            timeout_secs = timeout.as_secs(),
            force,
            "draining the daemon for a restart"
        );
        let pool = state.pool.clone();
        tokio::spawn(async move {
            let stop = || {
                crate::shutdown::mark_restart_requested();
                request_shutdown();
            };
            match crate::shutdown::restart_when_idle(
                &pool,
                requester_run,
                timeout,
                crate::shutdown::DRAIN_POLL,
                force,
                stop,
                &crate::hooks::run_is_parked,
            )
            .await
            {
                Ok(crate::shutdown::DrainOutcome::Stopped) => {
                    tracing::warn!("stopping the daemon to restart it");
                }
                Ok(crate::shutdown::DrainOutcome::TimedOut {
                    live_runs,
                    waiting_on,
                }) => {
                    tracing::warn!(
                        live_runs,
                        ?waiting_on,
                        "the restart drain timed out; the daemon carries on"
                    );
                }
                Err(error) => {
                    tracing::warn!(
                        %error,
                        "the restart drain could not count the live runs; the daemon carries on"
                    );
                }
            }
        });
    }

    crate::shutdown::readiness_for(&state.pool, requester_run, &crate::hooks::run_is_parked)
        .await
        .map(|readiness| (StatusCode::ACCEPTED, Json(readiness)))
        .map_err(|error| {
            tracing::warn!(%error, "could not read the readiness after starting the restart");
            StatusCode::INTERNAL_SERVER_ERROR
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse<T: serde::de::DeserializeOwned>(uri: &str) -> T {
        Query::<T>::try_from_uri(&uri.parse().unwrap()).unwrap().0
    }

    /// The query is how the CLI says who is asking and how long to wait.
    #[test]
    fn restart_reads_its_query() {
        let all: SystemRestartQuery =
            parse("/system/restart?timeout_secs=90&requester_run=42&force=true");
        assert_eq!(all.timeout_secs, Some(90));
        assert_eq!(all.requester_run, Some(42));
        assert_eq!(all.force, Some(true));

        let none: SystemRestartQuery = parse("/system/restart");
        assert_eq!(none.timeout_secs, None);
        assert_eq!(none.requester_run, None);
        assert_eq!(none.force, None);
    }

    /// The readiness the CLI polls takes the same `requester_run`.
    #[test]
    fn update_readiness_reads_the_requester_run() {
        let asked: super::super::admin::UpdateReadinessQuery =
            parse("/system/update-readiness?requester_run=7");
        assert_eq!(asked.requester_run, Some(7));
        let bare: super::super::admin::UpdateReadinessQuery = parse("/system/update-readiness");
        assert_eq!(bare.requester_run, None);
    }
}
