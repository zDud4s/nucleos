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
///
/// While a drain is already under way, `force=true` takes over and stops now; without `force` the
/// answer is 409 and nothing starts.
pub(super) async fn post_system_restart(
    State(state): State<AppState>,
    Query(query): Query<SystemRestartQuery>,
) -> Result<(StatusCode, Json<crate::shutdown::Readiness>), (StatusCode, Json<serde_json::Value>)> {
    let timeout = query
        .timeout_secs
        .map(std::time::Duration::from_secs)
        .unwrap_or(crate::shutdown::DEFAULT_DRAIN_TIMEOUT)
        .min(crate::shutdown::MAX_DRAIN_TIMEOUT);
    let requester_run = query.requester_run;
    let force = query.force.unwrap_or(false);

    match restart_start(!crate::shutdown::begin_draining(), force) {
        RestartStart::Refuse => {
            return Err((
                StatusCode::CONFLICT,
                Json(serde_json::json!({
                    "error": "a drain is already under way; wait for it or restart with force"
                })),
            ));
        }
        RestartStart::StopNow => {
            tracing::warn!("a forced restart takes over the drain under way");
            stop_for_restart();
        }
        RestartStart::Begin => {
            tracing::warn!(
                timeout_secs = timeout.as_secs(),
                force,
                "draining the daemon for a restart"
            );
            let pool = state.pool.clone();
            tokio::spawn(async move {
                match crate::shutdown::restart_when_idle(
                    &pool,
                    requester_run,
                    timeout,
                    crate::shutdown::DRAIN_POLL,
                    force,
                    stop_for_restart,
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
    }

    crate::shutdown::readiness_for(&state.pool, requester_run, &crate::hooks::run_is_parked)
        .await
        .map(|readiness| (StatusCode::ACCEPTED, Json(readiness)))
        .map_err(|error| {
            tracing::warn!(%error, "could not read the readiness after starting the restart");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": "could not read the readiness" })),
            )
        })
}

/// What a restart request does, given whether a drain is already running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RestartStart {
    /// No drain yet: start the wait (or, with `force`, stop at once).
    Begin,
    /// A drain is running and this request is forced: it wins, stop now.
    StopNow,
    /// A drain is running and this request is not forced: 409, nothing starts.
    Refuse,
}

/// PURE.
fn restart_start(already_draining: bool, force: bool) -> RestartStart {
    match (already_draining, force) {
        (false, _) => RestartStart::Begin,
        (true, true) => RestartStart::StopNow,
        (true, false) => RestartStart::Refuse,
    }
}

/// Spawns the relauncher if this daemon needs one, then marks the restart and stops. A failed spawn
/// leaves the daemon serving and reopens the door. Runs at most once while a stop is under way, so
/// a forced restart and the drain it took over cannot both spawn a relauncher.
fn stop_for_restart() {
    use std::sync::atomic::{AtomicBool, Ordering};
    static STARTED: AtomicBool = AtomicBool::new(false);
    if STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    match crate::relaunch::prepare_restart() {
        Ok(()) => {
            crate::shutdown::mark_restart_requested();
            request_shutdown();
        }
        Err(error) => {
            STARTED.store(false, Ordering::SeqCst);
            tracing::error!(%error, "not restarting: the daemon carries on");
            crate::shutdown::stop_draining();
        }
    }
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

    /// A second request while a drain runs: forced it wins, unforced it is refused (409).
    #[test]
    fn a_restart_during_a_drain_is_forced_through_or_refused() {
        assert_eq!(restart_start(false, false), RestartStart::Begin);
        assert_eq!(restart_start(false, true), RestartStart::Begin);
        assert_eq!(restart_start(true, true), RestartStart::StopNow);
        assert_eq!(restart_start(true, false), RestartStart::Refuse);
    }

    /// The handler answers 409 and leaves the draining state as it found it. (The forced path
    /// would spawn a relauncher and stop the process, so it is covered through `restart_start`.)
    #[tokio::test]
    async fn an_unforced_restart_during_a_drain_answers_409() {
        let state = super::super::tests::test_state().await;
        assert!(crate::shutdown::begin_draining(), "this thread's flag");
        let refused = post_system_restart(
            State(state),
            Query(SystemRestartQuery {
                timeout_secs: None,
                requester_run: None,
                force: None,
            }),
        )
        .await;
        crate::shutdown::stop_draining();
        let Err((status, Json(body))) = refused else {
            panic!("a restart during a drain without force must be refused");
        };
        assert_eq!(status, StatusCode::CONFLICT);
        assert!(
            body["error"]
                .as_str()
                .unwrap()
                .contains("already under way")
        );
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
