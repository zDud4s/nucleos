//! Draining the daemon before an update replaces it (plan `canal-de-atualizacoes`, phase 3).
//!
//! An installed copy that updates itself has to stop the daemon, and the daemon must not take
//! work down with it. The order is: the shell asks `GET /system/update-readiness`, the user
//! accepts, `POST /system/shutdown` puts the daemon in **draining**, the daemon waits until no run
//! is live, and only then does it leave through the same graceful path as `/daemon/shutdown`, with
//! exit code 0 so the autostart entry (`RestartOnFailure`, `Restart=on-failure`) does not bring it
//! back under the installer.
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

#[cfg(test)]
thread_local! {
    static DRAINING_IN_THIS_TEST: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

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

/// The answer to `GET /system/update-readiness`: which version this is, and whether it is safe to
/// stop it now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Readiness {
    pub version: &'static str,
    pub live_runs: i64,
    pub draining: bool,
}

pub async fn readiness(pool: &SqlitePool) -> sqlx::Result<Readiness> {
    Ok(Readiness {
        version: env!("CARGO_PKG_VERSION"),
        live_runs: live_runs(pool).await?,
        draining: is_draining(),
    })
}

/// How a drain ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DrainOutcome {
    /// No run was live on two reads in a row, and `stop` was called.
    Stopped,
    /// The deadline passed with runs still live. Nothing was stopped, nothing was interrupted, and
    /// the daemon is no longer draining.
    TimedOut { live_runs: i64 },
}

/// Waits until no run is live, then calls `stop`. The caller has already called
/// [`begin_draining`]; this clears the flag again if it gives up, so the daemon is never left
/// closed with nobody coming to stop it.
///
/// A database error also reopens the door: a daemon that cannot count its runs must not guess that
/// there are none.
pub async fn drain_until_idle(
    pool: &SqlitePool,
    timeout: Duration,
    poll: Duration,
    stop: impl FnOnce(),
) -> sqlx::Result<DrainOutcome> {
    let deadline = tokio::time::Instant::now() + timeout;
    let mut quiet_reads = 0u8;
    loop {
        let live = match live_runs(pool).await {
            Ok(live) => live,
            Err(error) => {
                stop_draining();
                return Err(error);
            }
        };
        if live == 0 {
            quiet_reads += 1;
            if quiet_reads >= 2 {
                stop();
                return Ok(DrainOutcome::Stopped);
            }
        } else {
            quiet_reads = 0;
            if tokio::time::Instant::now() >= deadline {
                stop_draining();
                return Ok(DrainOutcome::TimedOut { live_runs: live });
            }
        }
        tokio::time::sleep(poll).await;
    }
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
    use axum::routing::{get, post};
    use std::sync::Arc;
    use tower::ServiceExt;

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
        let outcome =
            drain_until_idle(&pool, Duration::from_secs(10), Duration::from_millis(10), {
                let (stops, finished, seen) = (
                    stops.clone(),
                    finished.clone(),
                    finished_when_stopped.clone(),
                );
                move || {
                    seen.store(finished.load(Ordering::SeqCst), Ordering::SeqCst);
                    stops.fetch_add(1, Ordering::SeqCst);
                }
            })
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
            Duration::from_millis(60),
            Duration::from_millis(10),
            {
                let stopped = stopped.clone();
                move || stopped.store(true, Ordering::SeqCst)
            },
        )
        .await
        .unwrap();

        assert_eq!(outcome, DrainOutcome::TimedOut { live_runs: 2 });
        assert!(!stopped.load(Ordering::SeqCst), "stopped under a live run");
        assert_eq!(status_of(&pool, run).await, "running");
        assert_eq!(status_of(&pool, waiting).await, "awaiting_approval");
        assert!(!is_draining(), "a daemon that gave up must serve again");
    }
}
