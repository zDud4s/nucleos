//! Draining the daemon before an update replaces it (plan `canal-de-atualizacoes`, phase 3).
//!
//! An installed copy that updates itself has to stop the daemon, and the daemon must not take
//! work down with it. The order is: the shell asks `GET /system/update-readiness`, the user
//! accepts, `POST /system/shutdown` puts the daemon in **draining**, the daemon waits until no run
//! is live, and only then does it leave through the same graceful path as `/daemon/shutdown`, with
//! exit code 0. That code is what makes the Windows supervisor (`--supervise`, see
//! `nucleos_core::supervise`) stop instead of relaunching the daemon; the logon task itself no
//! longer carries `RestartOnFailure`, which was measured inert. (systemd's `Restart=on-failure`
//! also reads the code, so there 0 matters too.)
//!
//! **Draining refuses new work; it never ends work.** A run is never marked `interrupted` here:
//! when the wait times out the daemon clears the flag and carries on, and says so.
//!
//! **Where "new work" is refused.** Runs are born in dozens of places, so there is no single point
//! to close. Two gates cover the ways work starts: `concurrency::room_for_on` (every claim of a
//! slot: worktree runs, jobs, waves, the scheduler, a resume) answers `NoRoom::Draining`, and
//! [`refuse_new_work`] answers 503 to the HTTP routes that start a run without a slot (a chat
//! turn, a preset, a team run, a project command). What survives both is a run born in the last
//! instants before exit; the wait therefore needs two quiet reads in a row before it stops, and
//! startup's orphan reconciliation remains the net for anything that still slips through.
//!
//! The state is a flag in memory and is never persisted: a daemon that restarts is not draining.
//! Judges nothing about WHAT to run; it only closes the door and counts what is inside.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use axum::extract::Request;
use axum::http::{HeaderValue, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use sqlx::SqlitePool;

use crate::execution::concurrency::LIVE_RUN_STATUSES;

// `live_runs` binds the two statuses by position; a third one must be bound there too.
const _: () = assert!(LIVE_RUN_STATUSES.len() == 2);

/// How long `POST /system/shutdown` waits for the live runs by default.
pub const DEFAULT_DRAIN_TIMEOUT: Duration = Duration::from_secs(30 * 60);
/// The longest wait a caller may ask for.
pub const MAX_DRAIN_TIMEOUT: Duration = Duration::from_secs(24 * 60 * 60);
/// How often the wait looks at the live runs.
pub const DRAIN_POLL: Duration = Duration::from_secs(2);

// The flag is process-wide in a real build. The library's own tests run in parallel threads of one
// process, and a test that drains must not close the door on its neighbours, so under `cfg(test)`
// it is per thread (every `#[tokio::test]` runs on its own current-thread runtime).
#[cfg(not(test))]
static DRAINING: AtomicBool = AtomicBool::new(false);

// The initializer IS `const`; clippy 1.97 still reports it inside the macro.
#[cfg(test)]
#[allow(clippy::missing_const_for_thread_local)]
mod draining_in_this_test {
    thread_local! {
        pub(super) static DRAINING_IN_THIS_TEST: std::cell::Cell<bool> =
            const { std::cell::Cell::new(false) };
    }
}
#[cfg(test)]
use draining_in_this_test::DRAINING_IN_THIS_TEST;

/// Whether the daemon is draining for an update.
#[cfg(not(test))]
pub fn is_draining() -> bool {
    DRAINING.load(Ordering::SeqCst)
}

/// Whether the daemon is draining for an update.
#[cfg(test)]
pub fn is_draining() -> bool {
    DRAINING_IN_THIS_TEST.with(|flag| flag.get())
}

/// Starts draining. `true` when this call is the one that started it, `false` when the daemon was
/// already draining, so two shutdown requests do not start two waits.
#[cfg(not(test))]
pub fn begin_draining() -> bool {
    !DRAINING.swap(true, Ordering::SeqCst)
}

/// Starts draining. `true` when this call is the one that started it.
#[cfg(test)]
pub fn begin_draining() -> bool {
    DRAINING_IN_THIS_TEST.with(|flag| !flag.replace(true))
}

/// Reopens the door: the wait gave up, so the daemon goes on serving.
#[cfg(not(test))]
pub fn stop_draining() {
    DRAINING.store(false, Ordering::SeqCst);
}

/// Reopens the door: the wait gave up, so the daemon goes on serving.
#[cfg(test)]
pub fn stop_draining() {
    DRAINING_IN_THIS_TEST.with(|flag| flag.set(false));
}

/// How many runs are live: the same two statuses a slot is held for (`LIVE_RUN_STATUSES`).
pub async fn live_runs(pool: &SqlitePool) -> sqlx::Result<i64> {
    sqlx::query_scalar("SELECT COUNT(*) FROM runs WHERE status IN (?, ?)")
        .bind(LIVE_RUN_STATUSES[0])
        .bind(LIVE_RUN_STATUSES[1])
        .fetch_one(pool)
        .await
}

/// A live run the restart drain is waiting on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BlockingRun {
    pub run_id: i64,
    pub chat_id: Option<String>,
}

/// Whether a run is parked on a question. Injected because the hooks' ask table lives in the app
/// crate, above this one.
pub type ParkedTest<'a> = &'a (dyn Fn(i64) -> bool + Send + Sync);

/// The live runs a restart has to wait for. Left out: a run parked on a question (it holds no work
/// that a restart loses, the question outlives it) and the requester. `NUCLEOS_RUN_ID` in a kept
/// process is the first turn's run but the same chat's, so the requester's whole chat is excluded;
/// a requester with no chat excludes only its own run. Runs parked in `awaiting_approval` still
/// count: the drain's time limit already settles that case.
pub async fn blocking_runs(
    pool: &SqlitePool,
    requester_run: Option<i64>,
    parked: ParkedTest<'_>,
) -> sqlx::Result<Vec<BlockingRun>> {
    let requester_chat: Option<String> = match requester_run {
        Some(run) => {
            sqlx::query_scalar::<_, Option<String>>("SELECT chat_id FROM runs WHERE id = ?")
                .bind(run)
                .fetch_optional(pool)
                .await?
                .flatten()
        }
        None => None,
    };
    let live: Vec<(i64, Option<String>)> =
        sqlx::query_as("SELECT id, chat_id FROM runs WHERE status IN (?, ?) ORDER BY id")
            .bind(LIVE_RUN_STATUSES[0])
            .bind(LIVE_RUN_STATUSES[1])
            .fetch_all(pool)
            .await?;
    Ok(live
        .into_iter()
        .filter(|(run_id, chat_id)| {
            let is_requester = match &requester_chat {
                Some(chat) => chat_id.as_deref() == Some(chat.as_str()),
                None => Some(*run_id) == requester_run,
            };
            !is_requester && !parked(*run_id)
        })
        .map(|(run_id, chat_id)| BlockingRun { run_id, chat_id })
        .collect())
}

/// How often the keep-awake loop re-reads whether a run is live.
const KEEP_AWAKE_POLL: Duration = Duration::from_secs(30);

/// How many failed counts in a row the loop rides out before it lets the hold go. One hiccup must
/// not drop a hold that work depends on, but a database that stays broken must not keep the machine
/// awake forever.
const KEEP_AWAKE_MAX_FAILED_COUNTS: u32 = 5;

/// One tick of the keep-awake loop: the answer for `hold_awake_while`, or `None` for "no change".
/// `failures` is the run of consecutive failed counts, owned by the caller.
fn keep_awake_step(count: Result<i64, ()>, failures: &mut u32) -> Option<bool> {
    match count {
        Ok(live) => {
            *failures = 0;
            Some(live > 0)
        }
        Err(()) => {
            *failures = failures.saturating_add(1);
            (*failures >= KEEP_AWAKE_MAX_FAILED_COUNTS).then_some(false)
        }
    }
}

/// Keeps the machine from idle-sleeping while [`live_runs`] is above zero. A failed count counts as
/// "no change" until [`KEEP_AWAKE_MAX_FAILED_COUNTS`] of them in a row, then the hold is released
/// (with a warning) and the next good count resumes normally.
pub async fn run_keep_awake_loop(pool: SqlitePool, enabled: bool) {
    let mut awake = crate::keep_awake::KeepAwake::system(enabled);
    let mut ticker = tokio::time::interval(KEEP_AWAKE_POLL);
    let mut failures = 0u32;
    loop {
        ticker.tick().await;
        let count = live_runs(&pool).await.map_err(|_| ());
        let was_failing = failures >= KEEP_AWAKE_MAX_FAILED_COUNTS;
        if let Some(has_work) = keep_awake_step(count, &mut failures) {
            if !has_work && failures >= KEEP_AWAKE_MAX_FAILED_COUNTS && !was_failing {
                tracing::warn!(
                    failures,
                    "keep-awake: the run count keeps failing; letting the machine sleep"
                );
            }
            awake.hold_awake_while(has_work);
        }
    }
}

/// The answer to `GET /system/update-readiness`: which version this is, and whether it is safe to
/// stop it now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Readiness {
    pub version: &'static str,
    pub live_runs: i64,
    pub draining: bool,
    /// What a restart would wait on right now. Additive: `live_runs` keeps its meaning.
    pub waiting_on: Vec<BlockingRun>,
}

/// The readiness with every live run counted as blocking (no requester, nothing parked).
pub async fn readiness(pool: &SqlitePool) -> sqlx::Result<Readiness> {
    readiness_for(pool, None, &|_| false).await
}

/// The readiness with `waiting_on` as a restart asked by `requester_run` would see it.
pub async fn readiness_for(
    pool: &SqlitePool,
    requester_run: Option<i64>,
    parked: ParkedTest<'_>,
) -> sqlx::Result<Readiness> {
    Ok(Readiness {
        version: env!("CARGO_PKG_VERSION"),
        live_runs: live_runs(pool).await?,
        draining: is_draining(),
        waiting_on: blocking_runs(pool, requester_run, parked).await?,
    })
}

/// How a drain ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DrainOutcome {
    /// Nothing blocked on two reads in a row, and `stop` was called.
    Stopped,
    /// The deadline passed with runs still blocking. Nothing was stopped, nothing was interrupted,
    /// and the daemon is no longer draining. `live_runs` is `waiting_on.len()`.
    TimedOut {
        live_runs: i64,
        waiting_on: Vec<BlockingRun>,
    },
}

/// Waits until no run blocks (see [`blocking_runs`]), then calls `stop`. The caller has already
/// called [`begin_draining`]; this clears the flag again if it gives up, so the daemon is never
/// left closed with nobody coming to stop it.
///
/// A database error also reopens the door: a daemon that cannot count its runs must not guess that
/// there are none.
pub async fn drain_until_idle(
    pool: &SqlitePool,
    requester_run: Option<i64>,
    timeout: Duration,
    poll: Duration,
    stop: impl FnOnce(),
    parked: ParkedTest<'_>,
) -> sqlx::Result<DrainOutcome> {
    let deadline = tokio::time::Instant::now() + timeout;
    let mut quiet_reads = 0u8;
    loop {
        let waiting_on = match blocking_runs(pool, requester_run, parked).await {
            Ok(waiting_on) => waiting_on,
            Err(error) => {
                stop_draining();
                return Err(error);
            }
        };
        if waiting_on.is_empty() {
            quiet_reads += 1;
            if quiet_reads >= 2 {
                stop();
                return Ok(DrainOutcome::Stopped);
            }
        } else {
            quiet_reads = 0;
            if tokio::time::Instant::now() >= deadline {
                stop_draining();
                return Ok(DrainOutcome::TimedOut {
                    live_runs: waiting_on.len() as i64,
                    waiting_on,
                });
            }
        }
        tokio::time::sleep(poll).await;
    }
}

/// Set when the daemon is leaving to be started again (`POST /system/restart`), so `main` picks the
/// exit that relaunches it instead of the plain stop. Never cleared: the process is on its way out.
static RESTART_REQUESTED: AtomicBool = AtomicBool::new(false);

/// Remembers that the stop in progress is a restart.
pub fn mark_restart_requested() {
    RESTART_REQUESTED.store(true, Ordering::SeqCst);
}

/// Whether the stop in progress is a restart.
pub fn restart_requested() -> bool {
    RESTART_REQUESTED.load(Ordering::SeqCst)
}

/// Waits like [`drain_until_idle`], or with `force` does not wait at all: `stop` is called at once
/// and the runs still live are left to the next start's orphan reconciliation. A timed-out wait
/// calls nothing and reopens the door, exactly as the update drain does.
pub async fn restart_when_idle(
    pool: &SqlitePool,
    requester_run: Option<i64>,
    timeout: Duration,
    poll: Duration,
    force: bool,
    stop: impl FnOnce(),
    parked: ParkedTest<'_>,
) -> sqlx::Result<DrainOutcome> {
    if force {
        stop();
        return Ok(DrainOutcome::Stopped);
    }
    drain_until_idle(pool, requester_run, timeout, poll, stop, parked).await
}

/// Whether a request starts a run without taking a slot, which `concurrency` cannot refuse for us.
/// Pure, over the concrete path, and deliberately a list: a route that is not here keeps working
/// while the daemon drains, which is what the hooks of a run still in flight need.
pub fn starts_work(method: &Method, path: &str) -> bool {
    if *method != Method::POST {
        return false;
    }
    let segments: Vec<&str> = path.trim_matches('/').split('/').collect();
    matches!(
        segments.as_slice(),
        ["runs"]
            | ["jobs"]
            | ["assistant", "message"]
            | ["concurrency", "waves"]
            | ["teams", _, "runs"]
            | ["presets", _, "run"]
            | ["projects", _, "commands", _, "run"]
            | ["assistant", "chats", _, "relay" | "say-now" | "forward"]
    )
}

/// The middleware: while draining, a request that starts work gets a 503 that says why.
pub async fn refuse_new_work(request: Request, next: Next) -> Response {
    if is_draining() && starts_work(request.method(), request.uri().path()) {
        let mut refusal = (
            StatusCode::SERVICE_UNAVAILABLE,
            axum::Json(serde_json::json!({
                "error": "draining",
                "message": "the daemon is draining to install an update and starts no new work",
            })),
        )
            .into_response();
        refusal
            .headers_mut()
            .insert(header::RETRY_AFTER, HeaderValue::from_static("30"));
        return refusal;
    }
    next.run(request).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution::concurrency::{ClaimOutcome, NoRoom, claim, room_for};
    use crate::execution::worktree::Owner;
    use axum::Router;
    use axum::body::Body;
    use axum::routing::post;
    use std::sync::Arc;
    use tower::ServiceExt;

    #[test]
    fn a_stuck_database_lets_the_hold_go() {
        let mut failures = 0;
        assert_eq!(keep_awake_step(Ok(2), &mut failures), Some(true));
        for _ in 1..KEEP_AWAKE_MAX_FAILED_COUNTS {
            assert_eq!(keep_awake_step(Err(()), &mut failures), None, "ridden out");
        }
        assert_eq!(keep_awake_step(Err(()), &mut failures), Some(false));
        assert_eq!(keep_awake_step(Ok(1), &mut failures), Some(true), "resumes");
        assert_eq!(failures, 0);
    }

    /// Reopens the door when a test ends, even by a failed assertion.
    struct Reopen;
    impl Drop for Reopen {
        fn drop(&mut self) {
            stop_draining();
        }
    }

    async fn seed_run(pool: &SqlitePool, status: &str) -> i64 {
        sqlx::query(
            "INSERT INTO runs (project_id, prompt, status, mode, created_at)
             VALUES ('project-a', 'a prompt', ?, 'worktree', '2026-10-19T00:00:00Z')",
        )
        .bind(status)
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid()
    }

    async fn status_of(pool: &SqlitePool, run: i64) -> String {
        sqlx::query_scalar("SELECT status FROM runs WHERE id = ?")
            .bind(run)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    /// The count the updater waits on: a running run and one waiting for approval are live, a run
    /// that ended or has not started is not. A wrong count either stops the daemon under a run or
    /// makes an update wait for nothing.
    #[tokio::test]
    async fn update_readiness_counts_only_live_runs() {
        let pool = crate::testdb::fresh_pool().await;
        for status in ["completed", "failed", "queued", "pending"] {
            seed_run(&pool, status).await;
        }
        let idle = readiness(&pool).await.unwrap();
        assert_eq!(idle.live_runs, 0);
        assert!(!idle.draining);
        assert_eq!(idle.version, env!("CARGO_PKG_VERSION"));

        seed_run(&pool, "running").await;
        seed_run(&pool, "awaiting_approval").await;
        assert_eq!(readiness(&pool).await.unwrap().live_runs, 2);
    }

    /// Draining closes both doors a new piece of work comes through, and leaves work already in
    /// flight alone: an owner that holds a slot gets it back, the hooks of a running agent still
    /// get through, and reads are untouched.
    #[tokio::test]
    async fn a_draining_daemon_refuses_a_new_run() {
        let pool = crate::testdb::fresh_pool().await;
        let held = seed_run(&pool, "running").await;
        let fresh = seed_run(&pool, "running").await;
        assert_eq!(
            claim(&pool, "project-a", Owner::Run(held)).await.unwrap(),
            ClaimOutcome::Claimed(0)
        );

        let _reopen = Reopen;
        assert!(begin_draining());
        assert!(
            !begin_draining(),
            "the second request must not start a second wait"
        );

        assert_eq!(
            claim(&pool, "project-a", Owner::Run(fresh)).await.unwrap(),
            ClaimOutcome::Full(NoRoom::Draining)
        );
        assert_eq!(
            room_for(&pool, "project-a").await.unwrap(),
            Some(NoRoom::Draining)
        );
        assert_eq!(
            claim(&pool, "project-a", Owner::Run(held)).await.unwrap(),
            ClaimOutcome::Claimed(0),
            "work in flight is never refused its own slot"
        );

        let app = Router::new()
            .route(
                "/runs",
                post(|| async { "started" }).get(|| async { "listed" }),
            )
            .route("/hooks/pretooluse", post(|| async { "hooked" }))
            .route("/assistant/message", post(|| async { "sent" }))
            .layer(axum::middleware::from_fn(refuse_new_work));
        let send = |method: &str, uri: &str| {
            let app = app.clone();
            let request = axum::http::Request::builder()
                .method(method)
                .uri(uri)
                .body(Body::empty())
                .unwrap();
            async move { app.oneshot(request).await.unwrap() }
        };
        for uri in ["/runs", "/assistant/message"] {
            let refused = send("POST", uri).await;
            assert_eq!(refused.status(), StatusCode::SERVICE_UNAVAILABLE, "{uri}");
            assert!(refused.headers().contains_key(header::RETRY_AFTER));
        }
        assert_eq!(send("GET", "/runs").await.status(), StatusCode::OK);
        assert_eq!(
            send("POST", "/hooks/pretooluse").await.status(),
            StatusCode::OK
        );

        stop_draining();
        assert_eq!(send("POST", "/runs").await.status(), StatusCode::OK);
        assert_eq!(
            claim(&pool, "project-a", Owner::Run(fresh)).await.unwrap(),
            ClaimOutcome::Claimed(1)
        );
    }

    /// A restart that cannot wait out the work leaves the daemon serving: nothing stopped, nothing
    /// interrupted, the door open again.
    #[tokio::test]
    async fn a_timed_out_restart_keeps_the_daemon_up_and_spawns_nothing() {
        let pool = crate::testdb::fresh_pool().await;
        let run = seed_run(&pool, "running").await;
        let _reopen = Reopen;
        assert!(begin_draining());
        let stopped = std::cell::Cell::new(false);
        let outcome = restart_when_idle(
            &pool,
            None,
            Duration::ZERO,
            Duration::from_millis(1),
            false,
            || stopped.set(true),
            &|_| false,
        )
        .await
        .unwrap();
        assert!(matches!(
            outcome,
            DrainOutcome::TimedOut { live_runs: 1, .. }
        ));
        assert!(!stopped.get(), "nothing is stopped");
        assert!(!is_draining(), "a later restart can start");
        assert_eq!(status_of(&pool, run).await, "running");
    }

    #[tokio::test]
    async fn force_restarts_without_waiting() {
        let pool = crate::testdb::fresh_pool().await;
        seed_run(&pool, "running").await;
        let stopped = std::cell::Cell::new(false);
        let outcome = restart_when_idle(
            &pool,
            None,
            Duration::from_secs(3600),
            Duration::from_secs(3600),
            true,
            || stopped.set(true),
            &|_| false,
        )
        .await
        .unwrap();
        assert_eq!(outcome, DrainOutcome::Stopped);
        assert!(stopped.get());
    }

    /// Running work holds a restart back until it settles; `force` is the way past it.
    #[tokio::test]
    async fn an_update_waits_for_running_work_and_can_be_forced() {
        let pool = crate::testdb::fresh_pool().await;
        let run = seed_run(&pool, "running").await;
        let stopped = std::cell::Cell::new(false);
        // Timing out, not cancelling: a cancelled query closes the only connection, and the
        // in-memory database with it.
        let waited = restart_when_idle(
            &pool,
            None,
            Duration::from_millis(60),
            Duration::from_millis(5),
            false,
            || stopped.set(true),
            &|_| false,
        )
        .await
        .unwrap();
        assert!(
            matches!(waited, DrainOutcome::TimedOut { .. }),
            "{waited:?}"
        );
        assert!(!stopped.get());

        // Settled: two quiet reads and it stops.
        sqlx::query("UPDATE runs SET status = 'completed' WHERE id = ?")
            .bind(run)
            .execute(&pool)
            .await
            .unwrap();
        let outcome = restart_when_idle(
            &pool,
            None,
            Duration::from_secs(3600),
            Duration::from_millis(5),
            false,
            || stopped.set(true),
            &|_| false,
        )
        .await
        .unwrap();
        assert_eq!(outcome, DrainOutcome::Stopped);
        assert!(stopped.get());
    }

    #[test]
    fn a_marked_restart_is_remembered() {
        mark_restart_requested();
        assert!(restart_requested());
    }

    #[test]
    fn only_routes_that_start_work_are_gated() {
        let gated = [
            "/runs",
            "/jobs",
            "/assistant/message",
            "/concurrency/waves",
            "/teams/t1/runs",
            "/presets/p1/run",
            "/projects/p1/commands/c1/run",
            "/assistant/chats/c1/relay",
            "/assistant/chats/c1/say-now",
            "/assistant/chats/c1/forward",
        ];
        for path in gated {
            assert!(starts_work(&Method::POST, path), "{path}");
            assert!(!starts_work(&Method::GET, path), "GET {path}");
        }
        for path in [
            "/runs/7/cancel",
            "/runs/7/message",
            "/hooks/pretooluse",
            "/hooks/ask-wait",
            "/system/shutdown",
            "/daemon/shutdown",
            "/health",
            "/assistant/turns/9/stop",
        ] {
            assert!(!starts_work(&Method::POST, path), "{path}");
        }
    }

    /// The daemon leaves only after the last live run is done, and only once that has held for two
    /// reads. `stop` is called exactly once, after the run finished.
    #[tokio::test]
    async fn shutdown_waits_for_live_runs_then_exits_zero() {
        let pool = crate::testdb::fresh_pool().await;
        let run = seed_run(&pool, "running").await;
        let _reopen = Reopen;
        assert!(begin_draining());

        let finished = Arc::new(AtomicBool::new(false));
        let finisher = {
            let (pool, finished) = (pool.clone(), finished.clone());
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(80)).await;
                sqlx::query("UPDATE runs SET status = 'completed' WHERE id = ?")
                    .bind(run)
                    .execute(&pool)
                    .await
                    .unwrap();
                finished.store(true, Ordering::SeqCst);
            })
        };

        let stops = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let finished_when_stopped = Arc::new(AtomicBool::new(false));
        let outcome = drain_until_idle(
            &pool,
            None,
            Duration::from_secs(10),
            Duration::from_millis(10),
            {
                let (stops, finished, seen) = (
                    stops.clone(),
                    finished.clone(),
                    finished_when_stopped.clone(),
                );
                move || {
                    seen.store(finished.load(Ordering::SeqCst), Ordering::SeqCst);
                    stops.fetch_add(1, Ordering::SeqCst);
                }
            },
            &|_| false,
        )
        .await
        .unwrap();
        finisher.await.unwrap();

        assert_eq!(outcome, DrainOutcome::Stopped);
        assert_eq!(stops.load(Ordering::SeqCst), 1);
        assert!(
            finished_when_stopped.load(Ordering::SeqCst),
            "the daemon stopped while a run was still live"
        );
    }

    /// When the wait gives up, the run is exactly as it was, nothing was stopped, and the daemon
    /// serves again. "Interrupted" is what a crash leaves behind, never what an update does.
    #[tokio::test]
    async fn a_drained_run_is_never_marked_interrupted() {
        let pool = crate::testdb::fresh_pool().await;
        let run = seed_run(&pool, "running").await;
        let waiting = seed_run(&pool, "awaiting_approval").await;
        let _reopen = Reopen;
        assert!(begin_draining());

        let stopped = Arc::new(AtomicBool::new(false));
        let outcome = drain_until_idle(
            &pool,
            None,
            Duration::from_millis(60),
            Duration::from_millis(10),
            {
                let stopped = stopped.clone();
                move || stopped.store(true, Ordering::SeqCst)
            },
            &|_| false,
        )
        .await
        .unwrap();

        assert!(matches!(
            outcome,
            DrainOutcome::TimedOut { live_runs: 2, .. }
        ));
        assert!(!stopped.load(Ordering::SeqCst), "stopped under a live run");
        assert_eq!(status_of(&pool, run).await, "running");
        assert_eq!(status_of(&pool, waiting).await, "awaiting_approval");
        assert!(!is_draining(), "a daemon that gave up must serve again");
    }

    async fn seed_chat_run(pool: &SqlitePool, status: &str, chat: &str) -> i64 {
        let run = seed_run(pool, status).await;
        sqlx::query("UPDATE runs SET chat_id = ? WHERE id = ?")
            .bind(chat)
            .bind(run)
            .execute(pool)
            .await
            .unwrap();
        run
    }

    /// A turn parked on a question holds nothing a restart would lose; the unparked one still does.
    #[tokio::test]
    async fn a_run_parked_on_a_question_does_not_hold_a_restart() {
        let pool = crate::testdb::fresh_pool().await;
        let parked = seed_chat_run(&pool, "running", "chat-a").await;
        let working = seed_chat_run(&pool, "running", "chat-b").await;

        let blocking = blocking_runs(&pool, None, &move |run| run == parked)
            .await
            .unwrap();
        assert_eq!(
            blocking,
            vec![BlockingRun {
                run_id: working,
                chat_id: Some("chat-b".to_owned())
            }]
        );
        assert!(
            blocking_runs(&pool, None, &|_| true)
                .await
                .unwrap()
                .is_empty(),
            "all parked: nothing holds the restart"
        );
    }

    /// The requester's `NUCLEOS_RUN_ID` is its first turn's; a later turn of the same chat is live
    /// under another id and is still the requester, so it must not hold the restart.
    #[tokio::test]
    async fn the_requesting_chat_does_not_hold_a_restart_even_with_a_stale_run_id() {
        let pool = crate::testdb::fresh_pool().await;
        let stale = seed_chat_run(&pool, "completed", "chat-a").await;
        let current = seed_chat_run(&pool, "running", "chat-a").await;
        assert_ne!(stale, current);

        assert!(
            blocking_runs(&pool, Some(stale), &|_| false)
                .await
                .unwrap()
                .is_empty()
        );
        // Without a chat only the run itself is excluded, so chat-a's turn counts again.
        let bare = seed_run(&pool, "running").await;
        let other = seed_run(&pool, "running").await;
        let blocking = blocking_runs(&pool, Some(bare), &|_| false).await.unwrap();
        assert_eq!(
            blocking.iter().map(|b| b.run_id).collect::<Vec<_>>(),
            vec![current, other]
        );
    }

    #[tokio::test]
    async fn another_chats_turn_holds_a_restart() {
        let pool = crate::testdb::fresh_pool().await;
        let mine = seed_chat_run(&pool, "running", "chat-a").await;
        let theirs = seed_chat_run(&pool, "running", "chat-b").await;

        let blocking = blocking_runs(&pool, Some(mine), &|_| false).await.unwrap();
        assert_eq!(
            blocking,
            vec![BlockingRun {
                run_id: theirs,
                chat_id: Some("chat-b".to_owned())
            }]
        );
    }

    #[tokio::test]
    async fn a_timed_out_drain_reports_what_it_waited_on() {
        let pool = crate::testdb::fresh_pool().await;
        let requester = seed_chat_run(&pool, "running", "chat-a").await;
        let theirs = seed_chat_run(&pool, "running", "chat-b").await;
        let _reopen = Reopen;
        assert!(begin_draining());

        let outcome = drain_until_idle(
            &pool,
            Some(requester),
            Duration::from_millis(60),
            Duration::from_millis(10),
            || panic!("stopped under a live run"),
            &|_| false,
        )
        .await
        .unwrap();

        assert_eq!(
            outcome,
            DrainOutcome::TimedOut {
                live_runs: 1,
                waiting_on: vec![BlockingRun {
                    run_id: theirs,
                    chat_id: Some("chat-b".to_owned())
                }]
            }
        );
        assert!(!is_draining());
    }
}
