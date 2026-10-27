//! Waking a run with the verdict of a verification it asked for.
//!
//! `verify::wake::spawn_watch` calls `deliver` once when a ticket asked for by `Caller::Run`
//! finishes after the hold, or `abandon` when it never will. A run that is still listening gets the verdict as a mid-run turn; a run that already
//! ended gets a continuation of its own session (`resume::resume_for_verify`). The bridge between
//! the two is the `verify_wakes` row: a run that ends with an unseen ticket opens a `pending` wake
//! (`open_for_exit`), which defers its judge-side work and keeps its worktree until the verdict is
//! delivered or refused.

use crate::state::AppState;
use crate::verify::Caller;
use crate::verify::plan::Ticket;

/// Verdict wakes per lineage root: a task that keeps asking for verification after it ended is
/// looping, and the owner is told when the fourth is refused.
pub(super) const WAKES_PER_LINEAGE: i64 = 3;

/// Verdict wakes per project in a rolling day, beside `judge::resolve::CORRECTIONS_PER_PROJECT_PER_DAY`
/// (a wake is a launch too, but one the owner did not have to ask for, so the ceiling is looser).
pub(super) const WAKES_PER_PROJECT_PER_DAY: i64 = 10;

const ST_PENDING: &str = "pending";
const ST_DELIVERED: &str = "delivered";
const ST_REFUSED: &str = "refused";

/// The text a run is woken with: the finished ticket as the CLI prints it, and the task the run
/// was working on, because a session resumed long after may have lost the thread.
///
/// Framed as coming from the daemon and as subordinate to the task, for the reason
/// `resume_instruction` gives: a bare instruction arriving out of nowhere reads as an injection.
pub fn verdict_prompt(ticket_id: i64, ticket: &serde_json::Value, original_task: &str) -> String {
    format!(
        "Verification ticket {ticket_id} finished.\n\n\
         This note comes from the daemon that launched this session: it is the verdict of the \
         verification you asked for, and it does not change your task.\n\n\
         {}\n\
         The task you were working on:\n{original_task}",
        crate::verify::render::render(ticket)
    )
}

/// Hands the finished `ticket` to run `run_id`. Called exactly once per ticket by the watcher, and
/// again by `deliver_ready` for a ticket that finished before its run did.
pub async fn deliver(state: &AppState, run_id: i64, ticket_id: i64, ticket: &Ticket) {
    tracing::debug!(
        run_id,
        ticket_id,
        done = ticket.done,
        "verify verdict ready for run"
    );
    #[cfg(test)]
    super::verify_wake::DELIVERED
        .lock()
        .unwrap()
        .push((run_id, ticket_id));
    if let Err(error) = settle(state, run_id, ticket_id, ticket).await {
        tracing::warn!(%error, run_id, ticket_id, "could not deliver a verification verdict");
    }
}

/// The seam P1's watcher test counts through.
#[cfg(test)]
pub(crate) static DELIVERED: std::sync::Mutex<Vec<(i64, i64)>> = std::sync::Mutex::new(Vec::new());

async fn settle(
    state: &AppState,
    run_id: i64,
    ticket_id: i64,
    ticket: &Ticket,
) -> sqlx::Result<()> {
    let pool = &state.pool;
    let seen: Option<Option<String>> =
        sqlx::query_scalar("SELECT seen_at FROM verify_requests WHERE id = ?")
            .bind(ticket_id)
            .fetch_optional(pool)
            .await?;
    match seen {
        None => return Ok(()),
        // The run read the verdict itself: nothing to push, and a wake opened at its exit is over.
        // That wake was holding the exit's judge-side work back, so it runs now, as a refusal's
        // does; a wake that was not pending has nothing left to run.
        Some(Some(_)) => {
            if set_state(pool, ticket_id, ST_PENDING, ST_DELIVERED).await?
                && !still_open(pool, run_id).await?
            {
                run_deferred_work(state, run_id).await;
            }
            return Ok(());
        }
        Some(None) => {}
    }
    let wake: Option<String> =
        sqlx::query_scalar("SELECT state FROM verify_wakes WHERE ticket_id = ?")
            .bind(ticket_id)
            .fetch_optional(pool)
            .await?;
    if matches!(wake.as_deref(), Some(ST_DELIVERED | ST_REFUSED)) {
        return Ok(());
    }
    let run: Option<(String, i64, String)> =
        sqlx::query_as("SELECT status, steerable, mode FROM runs WHERE id = ?")
            .bind(run_id)
            .fetch_optional(pool)
            .await?;
    let Some((status, steerable, mode)) = run else {
        return Ok(());
    };
    let value = serde_json::to_value(ticket)
        .map_err(|error| sqlx::Error::Protocol(format!("ticket is not serializable: {error}")))?;

    match status.as_str() {
        "running" => {
            // The same conditions `post_run_message` reads: only a run that asked to be steerable,
            // never a triage or toolless one. A run that is not listening gets its verdict when it
            // exits: `open_for_exit` opens the wake and `deliver_ready` delivers it.
            if steerable == 0
                || mode == crate::integrations::email::TRIAGE_MODE
                || super::tool_policy_for_mode(&mode) == crate::execution::runner::ToolPolicy::None
            {
                return Ok(());
            }
            let sender = state.run_messages.lock().unwrap().get(&run_id).cloned();
            let Some(sender) = sender else {
                return Ok(());
            };
            let task = super::original_task(pool, run_id).await?;
            let sent = sender.send(crate::execution::runner::LaterTurn {
                text: verdict_prompt(ticket_id, &value, &task),
                ..Default::default()
            });
            if sent.is_ok() {
                record(pool, run_id, ticket_id, ST_DELIVERED).await?;
            }
        }
        // Only a wake opened at the run's exit is a promise to continue it: without one the exit
        // already ran the judge-side work, and a resume now would run it twice.
        "completed" if wake.as_deref() == Some(ST_PENDING) => {
            match super::resume_for_verify(state, run_id, ticket_id, &value).await {
                Ok(child) => {
                    tracing::info!(
                        run_id,
                        ticket_id,
                        child,
                        "run resumed with its verification verdict"
                    )
                }
                Err(super::WakeRefusal::AlreadyHandled) => {}
                Err(refusal) => refuse(state, run_id, ticket_id, &refusal.reason()).await?,
            }
        }
        _ => {}
    }
    Ok(())
}

/// A refused wake: recorded, said in the feed, and the judge-side work the exit deferred is run now,
/// so the run ends exactly as it would have without a wake.
async fn refuse(state: &AppState, run_id: i64, ticket_id: i64, reason: &str) -> sqlx::Result<()> {
    let pool = &state.pool;
    record(pool, run_id, ticket_id, ST_REFUSED).await?;
    let project_id: Option<String> = sqlx::query_scalar("SELECT project_id FROM runs WHERE id = ?")
        .bind(run_id)
        .fetch_optional(pool)
        .await?
        .flatten();
    let _ = crate::feed::append(
        pool,
        project_id.as_deref(),
        "verify_wake_refused",
        &format!(
            "run {run_id} was not resumed for the verdict of verification ticket {ticket_id}: \
             {reason}. The verdict stays readable with verify_status"
        ),
        Some(run_id),
        Some(&crate::feed::run_subject(pool, run_id).await),
    )
    .await;
    if still_open(pool, run_id).await? {
        return Ok(());
    }
    run_deferred_work(state, run_id).await;
    Ok(())
}

#[cfg(test)]
thread_local! {
    /// How many times the deferred work ran on this thread: a `#[tokio::test]` runs on one.
    static DEFERRED: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// What the terminal arm skipped while a wake was open: the briefing sweep, the token-efficiency
/// observation and the judge's look at a failed gate.
async fn run_deferred_work(state: &AppState, run_id: i64) {
    #[cfg(test)]
    DEFERRED.with(|count| count.set(count.get() + 1));
    if let Err(error) = crate::brief::sweep(&state.pool).await {
        tracing::warn!(%error, run_id, "briefing credit not recorded");
    }
    if let Err(error) = crate::token_efficiency::observe_run(&state.pool, run_id).await {
        tracing::warn!(%error, run_id, "token efficiency not observed");
    }
    let gate: Option<(Option<String>, Option<i64>)> =
        sqlx::query_as("SELECT gate_status, gate_exit_code FROM runs WHERE id = ?")
            .bind(run_id)
            .fetch_optional(&state.pool)
            .await
            .unwrap_or(None);
    if let Some((Some(status), Some(exit_code))) = gate
        && status == "failed"
    {
        tokio::spawn(crate::judge::correction::after_gate_failed(
            state.clone(),
            run_id,
            exit_code as i32,
        ));
    }
}

/// Whether any wake of `run_id` is still `pending`.
async fn still_open(pool: &sqlx::SqlitePool, run_id: i64) -> sqlx::Result<bool> {
    sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM verify_wakes WHERE run_id = ? AND state = 'pending')",
    )
    .bind(run_id)
    .fetch_one(pool)
    .await
}

/// Upserts the wake row of `ticket_id` for `run_id` in `state`.
async fn record(
    pool: &sqlx::SqlitePool,
    run_id: i64,
    ticket_id: i64,
    state: &str,
) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO verify_wakes (ticket_id, run_id, root_run_id, state, created_at)
         SELECT ?, id, COALESCE(lineage_root_id, id), ?, ? FROM runs WHERE id = ? AND true
         ON CONFLICT(ticket_id) DO UPDATE SET state = excluded.state",
    )
    .bind(ticket_id)
    .bind(state)
    .bind(chrono::Utc::now().to_rfc3339())
    .bind(run_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Moves a wake from `from` to `to`; no row, or a row in another state, is left alone. Returns
/// whether a row moved.
async fn set_state(
    pool: &sqlx::SqlitePool,
    ticket_id: i64,
    from: &str,
    to: &str,
) -> sqlx::Result<bool> {
    let moved = sqlx::query("UPDATE verify_wakes SET state = ? WHERE ticket_id = ? AND state = ?")
        .bind(to)
        .bind(ticket_id)
        .bind(from)
        .execute(pool)
        .await?
        .rows_affected();
    Ok(moved > 0)
}

/// Settles the wake of a ticket the watcher can never deliver (it gave up, or the ticket is gone):
/// a `pending` wake is refused, which also runs the exit's deferred work, so the run does not stay
/// half-ended with its worktree kept. No pending wake, nothing to do.
pub async fn abandon(state: &AppState, run_id: i64, ticket_id: i64, reason: &str) {
    let pending: sqlx::Result<bool> = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM verify_wakes WHERE ticket_id = ? AND state = 'pending')",
    )
    .bind(ticket_id)
    .fetch_one(&state.pool)
    .await;
    match pending {
        Ok(true) => {
            if let Err(error) = refuse(state, run_id, ticket_id, reason).await {
                tracing::warn!(%error, run_id, ticket_id, "could not abandon a verify wake");
            }
        }
        Ok(false) => {}
        Err(error) => tracing::warn!(%error, run_id, ticket_id, "could not read a verify wake"),
    }
}

/// Called by the terminal arm when run `id` ended `completed` and that write won: opens a `pending`
/// wake for every ticket the run asked for and never saw the verdict of. Returns whether the run
/// has a wake open, in which case the arm skips its judge-side work (`run_deferred_work` does it
/// later if the wake is refused) and the worktree is kept (`GC_CANDIDATES_SQL`).
///
/// Only a worktree run outside a job is woken: a job node's run is driven by its job, and a second
/// writer in its tree would race the job's own gate.
pub async fn open_for_exit(state: &AppState, id: i64) -> bool {
    match open(&state.pool, id).await {
        Ok(open) => open,
        Err(error) => {
            tracing::warn!(%error, run_id = id, "could not open verify wakes for a finished run");
            false
        }
    }
}

async fn open(pool: &sqlx::SqlitePool, id: i64) -> sqlx::Result<bool> {
    let eligible: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM runs
         WHERE id = ? AND mode = 'worktree' AND job_id IS NULL AND session_id IS NOT NULL
           AND successor_run_id IS NULL)",
    )
    .bind(id)
    .fetch_one(pool)
    .await?;
    if !eligible {
        return Ok(false);
    }
    let tickets: Vec<i64> = sqlx::query_scalar(
        "SELECT id FROM verify_requests
         WHERE caller = ? AND seen_at IS NULL
           AND id NOT IN (SELECT ticket_id FROM verify_wakes)",
    )
    .bind(Caller::Run(id).label())
    .fetch_all(pool)
    .await?;
    for ticket in tickets {
        record(pool, id, ticket, ST_PENDING).await?;
    }
    still_open(pool, id).await
}

/// Delivers the verdict of every pending wake of run `id` whose ticket is already done: the ticket
/// finished before its run did, so the watcher has already come and gone. Spawned by the terminal
/// arm after the rest of its work, so a continuation never starts under the run's own handoff.
pub async fn deliver_ready(state: AppState, id: i64) {
    let pending: sqlx::Result<Vec<i64>> = sqlx::query_scalar(
        "SELECT ticket_id FROM verify_wakes WHERE run_id = ? AND state = 'pending'",
    )
    .bind(id)
    .fetch_all(&state.pool)
    .await;
    let Ok(pending) = pending else {
        return;
    };
    for ticket_id in pending {
        match crate::verify::read_ticket(&state.pool, ticket_id).await {
            Ok(Some((ticket, _))) if ticket.done => deliver(&state, id, ticket_id, &ticket).await,
            _ => {}
        }
    }
}

/// Whether a ticket created at `created_at` (RFC 3339) is older than the watcher's span. A stamp
/// that does not parse is not old: the watcher's own limit will end it.
fn past_watch_limit(created_at: &str, now: chrono::DateTime<chrono::Utc>) -> bool {
    chrono::DateTime::parse_from_rfc3339(created_at)
        .ok()
        .and_then(|created| (now - created.with_timezone(&chrono::Utc)).to_std().ok())
        .is_some_and(|age| age >= crate::verify::wake::WATCH_LIMIT)
}

/// Re-arms the watcher of every pending wake after a restart: the tasks that were waiting died with
/// the daemon, and the tickets themselves were requeued by `verify::runs::requeue_interrupted`.
/// A wake whose ticket is already older than the watcher's span is abandoned instead: re-watching
/// it would grant it another full span on every restart.
pub async fn rearm(state: &AppState) {
    let pending: sqlx::Result<Vec<(i64, i64, Option<String>)>> = sqlx::query_as(
        "SELECT w.ticket_id, w.run_id, r.created_at
         FROM verify_wakes w LEFT JOIN verify_requests r ON r.id = w.ticket_id
         WHERE w.state = 'pending'",
    )
    .fetch_all(&state.pool)
    .await;
    match pending {
        Ok(pending) => {
            let now = chrono::Utc::now();
            for (ticket_id, run_id, created_at) in pending {
                match created_at {
                    None => abandon(state, run_id, ticket_id, "its ticket no longer exists").await,
                    // An old ticket that finished while the daemon was down is still delivered;
                    // only one that never finished is given up on.
                    Some(created) if past_watch_limit(&created, now) => {
                        match crate::verify::wait_ticket(
                            &state.pool,
                            ticket_id,
                            std::time::Duration::ZERO,
                        )
                        .await
                        {
                            Ok(Some((ticket, _))) if ticket.done => {
                                deliver(state, run_id, ticket_id, &ticket).await;
                            }
                            _ => {
                                abandon(
                                    state,
                                    run_id,
                                    ticket_id,
                                    "its verification never finished (the daemon stopped waiting after 3 hours)",
                                )
                                .await;
                            }
                        }
                    }
                    Some(_) => crate::verify::wake::spawn_watch(state.clone(), ticket_id, run_id),
                }
            }
        }
        Err(error) => tracing::warn!(%error, "could not re-arm the pending verify wakes"),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::execution::runs::testkit::test_state_with_runner;
    use crate::verify::plan::Progress;

    /// A worktree run in `status` in a real repository, holding slot 0 and its tree. Returns the run
    /// and the repository's container, which must outlive the test.
    async fn seed_run(
        state: &AppState,
        status: &str,
        steerable: i64,
        session: &str,
        slot: i64,
    ) -> (i64, tempfile::TempDir) {
        let container =
            crate::execution::git_exec::testkit::space_free_tempdir("nucleos-verify-wake-");
        let root = container.path().join("repo");
        crate::execution::git_exec::testkit::initialize_repo(&root);
        let root = root.to_string_lossy().replace('\\', "/");
        let branch = crate::execution::git_exec::current_branch(
            std::path::Path::new(&root),
            std::time::Instant::now() + Duration::from_secs(60),
        )
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO autopilot_state (project_id, mode, project_root) VALUES ('proj', 'active', ?)
             ON CONFLICT(project_id) DO UPDATE SET project_root = excluded.project_root",
        )
        .bind(&root)
        .execute(&state.pool)
        .await
        .unwrap();
        let run = sqlx::query(
            "INSERT INTO runs (project_id, cwd, prompt, status, session_id, mode, steerable,
                               created_at, completed_at)
             VALUES ('proj', ?, 'Fix the flaky test in core', ?, ?, 'worktree', ?,
                     '2026-09-27T00:00:00Z', '2026-09-27T01:00:00Z')",
        )
        .bind(&root)
        .bind(status)
        .bind(session)
        .bind(steerable)
        .execute(&state.pool)
        .await
        .unwrap()
        .last_insert_rowid();
        sqlx::query(
            "INSERT INTO worktrees (owner_kind, owner_id, project_id, project_root, path, branch, created_at)
             VALUES ('run', ?, 'proj', ?, ?, ?, '2026-09-27T00:00:00Z')",
        )
        .bind(run)
        .bind(&root)
        .bind(&root)
        .bind(&branch)
        .execute(&state.pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO project_slots (project_id, slot, owner_kind, owner_id, claimed_at)
             VALUES ('proj', ?, 'run', ?, '2026-09-27T00:00:00Z')",
        )
        .bind(slot)
        .bind(run)
        .execute(&state.pool)
        .await
        .unwrap();
        (run, container)
    }

    /// A verification ticket asked for by `run`, optionally already read by it.
    async fn seed_ticket(state: &AppState, run: i64, seen: bool) -> i64 {
        sqlx::query(
            "INSERT INTO verify_requests (project_id, worktree, kind, scope, priority, caller,
                                          created_at, seen_at)
             VALUES ('proj', 'w', 'diff', 'core', 0, ?, '2026-09-27T01:00:00Z', ?)",
        )
        .bind(Caller::Run(run).label())
        .bind(seen.then_some("2026-09-27T01:30:00Z"))
        .execute(&state.pool)
        .await
        .unwrap()
        .last_insert_rowid()
    }

    fn finished(id: i64) -> Ticket {
        Ticket {
            ticket: id,
            done: true,
            verdict: Some("passed".to_owned()),
            project_id: "proj".to_owned(),
            worktree: "w".to_owned(),
            kind: "diff".to_owned(),
            scope: "core".to_owned(),
            base: None,
            note: None,
            unclaimed: Vec::new(),
            progress: Progress {
                total: 0,
                finished: 0,
                queued: 0,
                running: Vec::new(),
            },
            units: Vec::new(),
        }
    }

    async fn runs_count(state: &AppState) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM runs")
            .fetch_one(&state.pool)
            .await
            .unwrap()
    }

    async fn wake_state(state: &AppState, ticket: i64) -> Option<String> {
        sqlx::query_scalar("SELECT state FROM verify_wakes WHERE ticket_id = ?")
            .bind(ticket)
            .fetch_optional(&state.pool)
            .await
            .unwrap()
    }

    async fn refusal_rows(state: &AppState) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM feed WHERE kind = 'verify_wake_refused'")
            .fetch_one(&state.pool)
            .await
            .unwrap()
    }

    async fn wait_for_resume(runner: &crate::execution::runner::FakeCommandRunner) -> String {
        for _ in 0..300 {
            if let Some(session) = runner.last_resume.lock().unwrap().clone() {
                return session;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("the continuation never launched");
    }

    #[tokio::test]
    async fn seen_ticket_is_not_woken() {
        let (state, runner) =
            test_state_with_runner(Some(Duration::from_secs(5)), Duration::from_secs(600)).await;
        let (run, _repo) = seed_run(&state, "completed", 1, "sess-1", 0).await;
        let ticket = seed_ticket(&state, run, true).await;
        record(&state.pool, run, ticket, ST_PENDING).await.unwrap();

        deliver(&state, run, ticket, &finished(ticket)).await;

        assert_eq!(runs_count(&state).await, 1, "no continuation was written");
        assert!(runner.last_resume.lock().unwrap().is_none());
        // The wake the exit opened is over: the run read the verdict itself.
        assert_eq!(
            wake_state(&state, ticket).await.as_deref(),
            Some("delivered")
        );
    }

    fn deferred_runs() -> u32 {
        DEFERRED.with(std::cell::Cell::get)
    }

    #[tokio::test]
    async fn seen_wake_runs_deferred_work() {
        let (state, _runner) =
            test_state_with_runner(Some(Duration::from_secs(5)), Duration::from_secs(600)).await;
        let (run, _repo) = seed_run(&state, "completed", 1, "sess-1", 0).await;
        let ticket = seed_ticket(&state, run, true).await;
        record(&state.pool, run, ticket, ST_PENDING).await.unwrap();
        let before = deferred_runs();

        deliver(&state, run, ticket, &finished(ticket)).await;

        assert_eq!(deferred_runs(), before + 1, "the exit's deferred work ran");
        // A second delivery finds no pending wake and does not run it again.
        deliver(&state, run, ticket, &finished(ticket)).await;
        assert_eq!(deferred_runs(), before + 1);
    }

    #[tokio::test]
    async fn watcher_give_up_abandons_pending_wake() {
        let (state, runner) =
            test_state_with_runner(Some(Duration::from_secs(5)), Duration::from_secs(600)).await;
        let (run, _repo) = seed_run(&state, "completed", 1, "sess-1", 0).await;
        let ticket = seed_ticket(&state, run, false).await;
        record(&state.pool, run, ticket, ST_PENDING).await.unwrap();
        let before = deferred_runs();

        abandon(&state, run, ticket, "its verification never finished").await;

        assert_eq!(wake_state(&state, ticket).await.as_deref(), Some("refused"));
        assert_eq!(refusal_rows(&state).await, 1);
        assert_eq!(deferred_runs(), before + 1, "the deferred work ran");
        assert_eq!(runs_count(&state).await, 1, "nothing was resumed");
        assert!(runner.last_resume.lock().unwrap().is_none());
        // Nothing pending any more: abandoning again, or a ticket with no wake, does nothing.
        abandon(&state, run, ticket, "again").await;
        let bare = seed_ticket(&state, run, false).await;
        abandon(&state, run, bare, "no wake").await;
        assert_eq!(deferred_runs(), before + 1);
        assert_eq!(wake_state(&state, bare).await, None);
    }

    #[test]
    fn rearm_abandons_wakes_older_than_the_watch_limit() {
        let now = chrono::Utc::now();
        let stamp = |ago: Duration| (now - chrono::Duration::from_std(ago).unwrap()).to_rfc3339();
        let limit = crate::verify::wake::WATCH_LIMIT;
        assert!(past_watch_limit(
            &stamp(limit + Duration::from_secs(60)),
            now
        ));
        assert!(!past_watch_limit(&stamp(Duration::from_secs(60)), now));
        assert!(!past_watch_limit("not a date", now));
    }

    /// An old pending wake is given up on only when its ticket never finished; one whose ticket
    /// finished while the daemon was down is still delivered.
    #[tokio::test]
    async fn rearm_abandons_an_old_pending_wake() {
        let (state, _runner) =
            test_state_with_runner(Some(Duration::from_secs(5)), Duration::from_secs(600)).await;
        // `seed_ticket` stamps 2026-09-27, long before any 3 h span ends.
        let (run, _repo) = seed_run(&state, "completed", 1, "sess-1", 0).await;
        let ticket = seed_ticket(&state, run, false).await;
        // A unit whose verify run is still running: the ticket is not done.
        sqlx::query(
            "INSERT INTO verify_runs (id, worktree, scope, origin, requested_by, argv, status)
             VALUES (900, 'w', 'core', 'request', 'owner', '[\"run\"]', 'running')",
        )
        .execute(&state.pool)
        .await
        .unwrap();
        sqlx::query(
            r#"UPDATE verify_requests SET plan = '[{"group":"core","argv":["run","core"],
               "why":"paths: a","fingerprint":null,"cacheable":true,"run_id":900,
               "cached_from":null,"skipped":null}]' WHERE id = ?"#,
        )
        .bind(ticket)
        .execute(&state.pool)
        .await
        .unwrap();
        record(&state.pool, run, ticket, ST_PENDING).await.unwrap();

        rearm(&state).await;

        assert_eq!(wake_state(&state, ticket).await.as_deref(), Some("refused"));

        // An empty plan is a finished ticket: the same old wake is delivered instead.
        let (done_run, _done_repo) = seed_run(&state, "completed", 1, "sess-2", 1).await;
        let done = seed_ticket(&state, done_run, false).await;
        record(&state.pool, done_run, done, ST_PENDING)
            .await
            .unwrap();

        rearm(&state).await;

        assert_eq!(wake_state(&state, done).await.as_deref(), Some("delivered"));
    }

    #[tokio::test]
    async fn live_steerable_run_gets_later_turn() {
        let (state, _runner) =
            test_state_with_runner(Some(Duration::from_secs(5)), Duration::from_secs(600)).await;
        let (run, _repo) = seed_run(&state, "running", 1, "sess-1", 0).await;
        let ticket = seed_ticket(&state, run, false).await;
        let (sender, mut turns) = tokio::sync::mpsc::unbounded_channel();
        state.run_messages.lock().unwrap().insert(run, sender);

        deliver(&state, run, ticket, &finished(ticket)).await;

        let turn = turns.try_recv().expect("the run was sent a turn");
        assert!(
            turn.text
                .starts_with(&format!("Verification ticket {ticket} finished."))
        );
        assert!(turn.text.contains("verdict: passed"));
        assert!(turn.text.contains("Fix the flaky test in core"));
        assert!(!turn.interrupt && turn.images.is_empty());
        assert_eq!(
            wake_state(&state, ticket).await.as_deref(),
            Some("delivered")
        );
        assert_eq!(runs_count(&state).await, 1, "a live run is not resumed");
    }

    #[tokio::test]
    async fn exited_run_resumes_with_verdict_prompt() {
        let (state, runner) =
            test_state_with_runner(Some(Duration::from_millis(200)), Duration::from_secs(600))
                .await;
        let (run, _repo) = seed_run(&state, "completed", 1, "sess-1", 0).await;
        let ticket = seed_ticket(&state, run, false).await;
        record(&state.pool, run, ticket, ST_PENDING).await.unwrap();

        deliver(&state, run, ticket, &finished(ticket)).await;

        let child: i64 = sqlx::query_scalar("SELECT id FROM runs WHERE id != ?")
            .bind(run)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        // The child's own `session_id` is not asserted: the fake runner overwrites it as soon as
        // the child starts. Which session was resumed is `wait_for_resume` below.
        let (prompt, root): (String, Option<i64>) =
            sqlx::query_as("SELECT prompt, lineage_root_id FROM runs WHERE id = ?")
                .bind(child)
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert!(prompt.starts_with(&format!("Verification ticket {ticket} finished.")));
        assert!(prompt.contains("verdict: passed"));
        assert!(prompt.ends_with("Fix the flaky test in core"));
        assert_eq!(root, Some(run));
        let (tree, slot): (i64, i64) = sqlx::query_as(
            "SELECT (SELECT owner_id FROM worktrees), (SELECT owner_id FROM project_slots)",
        )
        .fetch_one(&state.pool)
        .await
        .unwrap();
        assert_eq!((tree, slot), (child, child));
        assert_eq!(
            wake_state(&state, ticket).await.as_deref(),
            Some("delivered")
        );
        assert_eq!(wait_for_resume(&runner).await, "sess-1");
        // A second delivery of the same ticket (the exit's and the watcher's) writes nothing more.
        deliver(&state, run, ticket, &finished(ticket)).await;
        assert_eq!(runs_count(&state).await, 2);
    }

    /// Which CLI continues a session is decided by `resume_session_id` alone: the Codex runner turns
    /// it into `codex exec resume <id>` (`runner/codex.rs`), so an origin whose session is a Codex
    /// thread id takes the same route as a Claude one, with the id passed through untouched.
    #[tokio::test]
    async fn codex_run_resumes_via_exec_resume_route() {
        let (state, runner) =
            test_state_with_runner(Some(Duration::from_millis(200)), Duration::from_secs(600))
                .await;
        let thread = "0199a213-81c0-7800-8aa1-bbab2a035a53";
        let (run, _repo) = seed_run(&state, "completed", 0, thread, 0).await;
        let ticket = seed_ticket(&state, run, false).await;
        record(&state.pool, run, ticket, ST_PENDING).await.unwrap();

        deliver(&state, run, ticket, &finished(ticket)).await;

        assert_eq!(wait_for_resume(&runner).await, thread);
        assert_eq!(
            wake_state(&state, ticket).await.as_deref(),
            Some("delivered")
        );
    }

    #[tokio::test]
    async fn exit_with_open_ticket_defers_judge_and_gc() {
        let (state, _runner) =
            test_state_with_runner(Some(Duration::from_secs(5)), Duration::from_secs(600)).await;
        let (run, _repo) = seed_run(&state, "completed", 1, "sess-1", 0).await;
        let (quiet, _quiet_repo) = seed_run(&state, "completed", 1, "sess-2", 1).await;
        let ticket = seed_ticket(&state, run, false).await;
        let collectable = || async {
            crate::execution::worktree::gc_candidates(
                &state.pool,
                chrono::Utc::now(),
                chrono::Duration::hours(1),
            )
            .await
            .unwrap()
            .iter()
            .map(|row| row.owner_id)
            .collect::<Vec<_>>()
        };

        assert!(
            open_for_exit(&state, run).await,
            "an unseen ticket opens a wake"
        );
        assert!(
            !open_for_exit(&state, quiet).await,
            "a run with no ticket opens none"
        );
        assert_eq!(wake_state(&state, ticket).await.as_deref(), Some("pending"));
        let kept = collectable().await;
        assert!(
            !kept.contains(&run),
            "the tree of a run with a pending wake is kept"
        );
        assert!(kept.contains(&quiet));

        // Settled, the tree is collectable again.
        set_state(&state.pool, ticket, ST_PENDING, ST_DELIVERED)
            .await
            .unwrap();
        assert!(collectable().await.contains(&run));
        // And a ticket the run already read opens nothing.
        let read = seed_ticket(&state, quiet, true).await;
        assert!(!open_for_exit(&state, quiet).await);
        assert_eq!(wake_state(&state, read).await, None);
    }

    #[tokio::test]
    async fn fourth_wake_in_lineage_refused() {
        let (state, _runner) =
            test_state_with_runner(Some(Duration::from_secs(5)), Duration::from_secs(600)).await;
        let (run, _repo) = seed_run(&state, "completed", 1, "sess-1", 0).await;
        for ticket in 901..=903 {
            sqlx::query(
                "INSERT INTO verify_wakes (ticket_id, run_id, root_run_id, state, created_at)
                 VALUES (?, ?, ?, 'delivered', '2026-09-27T01:00:00Z')",
            )
            .bind(ticket)
            .bind(run)
            .bind(run)
            .execute(&state.pool)
            .await
            .unwrap();
        }
        let ticket = seed_ticket(&state, run, false).await;
        record(&state.pool, run, ticket, ST_PENDING).await.unwrap();

        deliver(&state, run, ticket, &finished(ticket)).await;

        assert_eq!(runs_count(&state).await, 1, "no fourth continuation");
        assert_eq!(wake_state(&state, ticket).await.as_deref(), Some("refused"));
        assert_eq!(refusal_rows(&state).await, 1);
        let direct =
            super::super::resume_for_verify(&state, run, ticket + 1, &serde_json::json!({})).await;
        assert!(matches!(direct, Err(super::super::WakeRefusal::LineageCap)));
    }

    #[tokio::test]
    async fn kill_switch_blocks_wake() {
        let (state, _runner) =
            test_state_with_runner(Some(Duration::from_secs(5)), Duration::from_secs(600)).await;
        let (run, _repo) = seed_run(&state, "completed", 1, "sess-1", 0).await;
        let ticket = seed_ticket(&state, run, false).await;
        record(&state.pool, run, ticket, ST_PENDING).await.unwrap();
        crate::governance::autopilot::set_kill_switch(&state.pool, true)
            .await
            .unwrap();

        deliver(&state, run, ticket, &finished(ticket)).await;

        assert_eq!(runs_count(&state).await, 1);
        assert_eq!(wake_state(&state, ticket).await.as_deref(), Some("refused"));
        assert_eq!(refusal_rows(&state).await, 1);
        let direct =
            super::super::resume_for_verify(&state, run, ticket, &serde_json::json!({})).await;
        assert!(matches!(direct, Err(super::super::WakeRefusal::KillSwitch)));
    }

    #[tokio::test]
    async fn budget_ceiling_blocks_wake() {
        let (state, _runner) =
            test_state_with_runner(Some(Duration::from_secs(5)), Duration::from_secs(600)).await;
        let (run, _repo) = seed_run(&state, "completed", 1, "sess-1", 0).await;
        let ticket = seed_ticket(&state, run, false).await;
        record(&state.pool, run, ticket, ST_PENDING).await.unwrap();
        crate::governance::budget::set_budget_config(
            &state.pool,
            &crate::governance::budget::BudgetConfig {
                limit_usd: Some(0.01),
                period: crate::governance::budget::BudgetPeriod::Daily,
                hourly_limit_usd: None,
                per_run_reserve_usd: 5.0,
                time_cost_per_hour_usd: 0.0,
            },
        )
        .await
        .unwrap();

        deliver(&state, run, ticket, &finished(ticket)).await;

        assert_eq!(runs_count(&state).await, 1);
        assert_eq!(wake_state(&state, ticket).await.as_deref(), Some("refused"));
        assert_eq!(refusal_rows(&state).await, 1);
        let direct =
            super::super::resume_for_verify(&state, run, ticket, &serde_json::json!({})).await;
        assert!(matches!(direct, Err(super::super::WakeRefusal::Brake(_))));
    }
}
