//! One row per verification unit the daemon ran or skipped.
//!
//! Spec `2026-10-05-selecao-de-testes-design.md`, "Registo". F0 records the three gates the daemon
//! already runs (a job item, a run, a gated merge) so that every later phase has a baseline to be
//! measured against. Recording is best effort: a row that cannot be written is a warning, never a
//! different verdict.

use std::path::Path;
use std::time::{Duration, Instant};

use sqlx::SqlitePool;

use crate::gate::GateOutcome;

pub const ORIGIN_JOB_ITEM: &str = "job_item";
pub const ORIGIN_RUN: &str = "run";
pub const ORIGIN_MERGE: &str = "merge";
/// A unit asked for through `verify`; `origin_id` is the `verify_requests` id (spec section 7).
pub const ORIGIN_VERIFY: &str = "verify";

pub const SCOPE_FULL: &str = "full";
pub const REQUESTED_BY_GATE: &str = "gate";

pub const STATUS_PASSED: &str = "passed";
pub const STATUS_FAILED: &str = "failed";
pub const STATUS_ERRORED: &str = "errored";
/// A unit that did not run because a green with the same fingerprint is still fresh (spec section 5.3).
pub const STATUS_SKIPPED_CACHED: &str = "skipped_cached";
#[cfg_attr(not(test), allow(dead_code))]
pub const STATUS_QUEUED: &str = "queued";
#[cfg_attr(not(test), allow(dead_code))]
pub const STATUS_RUNNING: &str = "running";

#[cfg_attr(not(test), allow(dead_code))]
pub const PRIORITY_INTERACTIVE: i64 = 0;
pub const PRIORITY_AUTONOMOUS: i64 = 1;
pub const PRIORITY_POSTGATE: i64 = 2;

/// A row interrupted by this many daemon restarts is given up on, so a request that brings the
/// daemon down does not loop forever.
#[cfg_attr(not(test), allow(dead_code))]
pub const MAX_INTERRUPTIONS: i64 = 3;

/// The gate's output is already capped at 1 MiB (`gate::GATE_OUTPUT_CAP`); a row keeps only what a
/// person reads to see why it went red.
const OUTPUT_TAIL_CHARS: usize = 4000;

/// How long reading the worktree's HEAD may take before the row goes without it.
const SHA_TIMEOUT: Duration = Duration::from_secs(10);

/// Who asked for a gate, as the row records it.
pub struct GateContext<'a> {
    pub project_id: Option<&'a str>,
    pub origin: &'static str,
    /// The job, run or vcs request id.
    pub origin_id: Option<i64>,
    /// The item's position, for a job item gate.
    pub ordinal: Option<i64>,
}

pub struct Row<'a> {
    pub project_id: Option<&'a str>,
    pub worktree: &'a str,
    pub sha: Option<&'a str>,
    pub scope: &'a str,
    pub origin: &'a str,
    pub origin_id: Option<i64>,
    pub ordinal: Option<i64>,
    pub requested_by: &'a str,
    pub group_name: Option<&'a str>,
    pub kind: Option<&'a str>,
    pub argv: &'a str,
    pub fingerprint: Option<&'a str>,
    pub status: &'a str,
    pub exit_code: Option<i64>,
    pub duration_ms: i64,
    pub started_at: &'a str,
    pub finished_at: &'a str,
    pub output_tail: Option<&'a str>,
}

pub async fn record(pool: &SqlitePool, row: &Row<'_>) -> Result<i64, sqlx::Error> {
    let id = sqlx::query(
        "INSERT INTO verify_runs (project_id, worktree, sha, scope, origin, origin_id, ordinal, \
         requested_by, group_name, kind, argv, fingerprint, status, exit_code, duration_ms, \
         started_at, finished_at, output_tail) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(row.project_id)
    .bind(row.worktree)
    .bind(row.sha)
    .bind(row.scope)
    .bind(row.origin)
    .bind(row.origin_id)
    .bind(row.ordinal)
    .bind(row.requested_by)
    .bind(row.group_name)
    .bind(row.kind)
    .bind(row.argv)
    .bind(row.fingerprint)
    .bind(row.status)
    .bind(row.exit_code)
    .bind(row.duration_ms)
    .bind(row.started_at)
    .bind(row.finished_at)
    .bind(row.output_tail)
    .execute(pool)
    .await?
    .last_insert_rowid();
    Ok(id)
}

/// `gate::run_gate`, timed and recorded. The verdict returned is exactly the one `run_gate` gave.
///
/// `pool` is optional for the queue's sake: a `GitExecutor` built by a test has none, and must
/// measure merges exactly as before.
pub async fn timed_gate(
    pool: Option<&SqlitePool>,
    ctx: GateContext<'_>,
    worktree: &Path,
    project_root: &Path,
    command: &str,
    timeout: Duration,
) -> GateOutcome {
    // Read before the gate runs: it is the tree being measured, whatever the gate leaves behind.
    let sha = match pool {
        Some(_) => head_sha(worktree).await,
        None => None,
    };
    let started_at = chrono::Utc::now().to_rfc3339();
    let clock = Instant::now();
    let outcome = crate::gate::run_gate(worktree, project_root, command, timeout).await;
    let Some(pool) = pool else {
        return outcome;
    };
    let duration_ms = i64::try_from(clock.elapsed().as_millis()).unwrap_or(i64::MAX);
    let finished_at = chrono::Utc::now().to_rfc3339();
    let (status, exit_code, output_tail) = match &outcome {
        GateOutcome::Passed => (STATUS_PASSED, Some(0), None),
        GateOutcome::Failed { exit_code, output } => (
            STATUS_FAILED,
            Some(i64::from(*exit_code)),
            Some(tail(output)),
        ),
        GateOutcome::Errored { reason } => (STATUS_ERRORED, None, Some(tail(reason))),
    };
    let worktree_text = worktree.to_string_lossy();
    let row = Row {
        project_id: ctx.project_id,
        worktree: &worktree_text,
        sha: sha.as_deref(),
        scope: SCOPE_FULL,
        origin: ctx.origin,
        origin_id: ctx.origin_id,
        ordinal: ctx.ordinal,
        requested_by: REQUESTED_BY_GATE,
        group_name: None,
        kind: None,
        argv: command,
        fingerprint: None,
        status,
        exit_code,
        duration_ms,
        started_at: &started_at,
        finished_at: &finished_at,
        output_tail: output_tail.as_deref(),
    };
    if let Err(error) = record(pool, &row).await {
        tracing::warn!(origin = ctx.origin, origin_id = ?ctx.origin_id, %error,
            "could not record the gate in verify_runs; the verdict stands");
    }
    outcome
}

/// The worktree's HEAD, or `None` when it is not a repository or git did not answer in time.
async fn head_sha(worktree: &Path) -> Option<String> {
    let args = [
        std::ffi::OsStr::new("rev-parse"),
        std::ffi::OsStr::new("HEAD"),
    ];
    let result = crate::git_exec::run_git(worktree, &args, SHA_TIMEOUT)
        .await
        .ok()?;
    if result.exit_code != Some(0) {
        return None;
    }
    let sha = result.stdout.trim();
    (!sha.is_empty()).then(|| sha.to_owned())
}

/// The last `OUTPUT_TAIL_CHARS` characters — never a cut through a UTF-8 sequence.
fn tail(text: &str) -> String {
    let count = text.chars().count();
    text.chars()
        .skip(count.saturating_sub(OUTPUT_TAIL_CHARS))
        .collect()
}

/// A unit of verification a caller asks the executor to run.
#[cfg_attr(not(test), allow(dead_code))]
pub struct Request {
    pub project_id: Option<String>,
    pub worktree: String,
    pub scope: String,
    pub origin: String,
    pub origin_id: Option<i64>,
    pub requested_by: String,
    pub group_name: Option<String>,
    pub kind: Option<String>,
    pub argv: Vec<String>,
    pub fingerprint: Option<String>,
    pub priority: i64,
    pub weight: i64,
    pub timeout_ms: i64,
}

/// What `enqueue` answers: the row's id, and whether it joined one already in flight.
#[cfg_attr(not(test), allow(dead_code))]
pub struct Submitted {
    pub id: i64,
    pub joined: bool,
}

/// A queued row as the scheduler needs it.
#[cfg_attr(not(test), allow(dead_code))]
pub struct Queued {
    pub id: i64,
    pub project_id: Option<String>,
    pub priority: i64,
    pub weight: i64,
    pub enqueued_ms: i64,
}

/// A row the executor claimed and must now run.
#[cfg_attr(not(test), allow(dead_code))]
pub struct Claimed {
    pub id: i64,
    pub project_id: Option<String>,
    pub worktree: String,
    pub argv: Vec<String>,
    pub weight: i64,
    pub timeout_ms: i64,
}

/// What a reader sees of one row.
#[cfg_attr(not(test), allow(dead_code))]
pub struct State {
    // Read from F2a-2 (verify); until then nothing reads it.
    #[allow(dead_code)]
    pub id: i64,
    pub status: String,
    pub exit_code: Option<i64>,
    pub duration_ms: Option<i64>,
    pub output_tail: Option<String>,
    pub interruptions: i64,
}

/// Queues a request, or joins an equal one that is still queued or running.
///
/// Joining needs a `fingerprint`: without one two requests are never known to be the same. A join
/// creates no row and raises the queued row's priority if the new request is more urgent. The
/// origin of the joined request is not recorded (v1 limitation).
#[cfg_attr(not(test), allow(dead_code))]
pub async fn enqueue(pool: &SqlitePool, request: &Request, now_ms: i64) -> sqlx::Result<Submitted> {
    let argv = serde_json::to_string(&request.argv).unwrap_or_else(|_| "[]".to_owned());
    // IMMEDIATE takes the write lock up front: in a deferred transaction two equal submits could
    // both run the join SELECT, both miss, and both insert (or the second would get SQLITE_BUSY).
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    if let Some(fingerprint) = &request.fingerprint {
        let existing: Option<i64> = sqlx::query_scalar(
            "SELECT id FROM verify_runs WHERE status IN ('queued', 'running') \
             AND project_id IS ? AND worktree = ? AND group_name IS ? AND kind IS ? \
             AND argv = ? AND fingerprint = ? ORDER BY id LIMIT 1",
        )
        .bind(&request.project_id)
        .bind(&request.worktree)
        .bind(&request.group_name)
        .bind(&request.kind)
        .bind(&argv)
        .bind(fingerprint)
        .fetch_optional(&mut *tx)
        .await?;
        if let Some(id) = existing {
            sqlx::query(
                "UPDATE verify_runs SET priority = MIN(priority, ?) \
                 WHERE id = ? AND status = 'queued'",
            )
            .bind(request.priority)
            .bind(id)
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
            return Ok(Submitted { id, joined: true });
        }
    }
    let id = sqlx::query(
        "INSERT INTO verify_runs (project_id, worktree, scope, origin, origin_id, requested_by, \
         group_name, kind, argv, fingerprint, status, priority, weight, timeout_ms, enqueued_ms) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 'queued', ?, ?, ?, ?)",
    )
    .bind(&request.project_id)
    .bind(&request.worktree)
    .bind(&request.scope)
    .bind(&request.origin)
    .bind(request.origin_id)
    .bind(&request.requested_by)
    .bind(&request.group_name)
    .bind(&request.kind)
    .bind(&argv)
    .bind(&request.fingerprint)
    .bind(request.priority)
    .bind(request.weight)
    .bind(request.timeout_ms)
    .bind(now_ms)
    .execute(&mut *tx)
    .await?
    .last_insert_rowid();
    tx.commit().await?;
    Ok(Submitted { id, joined: false })
}

/// Every queued row, oldest first, for the scheduler to choose from.
#[cfg_attr(not(test), allow(dead_code))]
pub async fn queued(pool: &SqlitePool) -> sqlx::Result<Vec<Queued>> {
    // The tuple is the `SELECT`'s own shape and lives only until the `map` below builds `Queued`;
    // a named struct would repeat the column order in two places.
    #[allow(clippy::type_complexity)]
    let rows: Vec<(i64, Option<String>, i64, i64, Option<i64>)> = sqlx::query_as(
        "SELECT id, project_id, priority, weight, enqueued_ms FROM verify_runs \
         WHERE status = 'queued' ORDER BY enqueued_ms, id",
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(id, project_id, priority, weight, enqueued_ms)| Queued {
            id,
            project_id,
            priority,
            weight,
            enqueued_ms: enqueued_ms.unwrap_or(0),
        })
        .collect())
}

/// Takes a queued row for running. `None` when it is no longer queued.
#[cfg_attr(not(test), allow(dead_code))]
pub async fn claim(pool: &SqlitePool, id: i64) -> sqlx::Result<Option<Claimed>> {
    let started_at = chrono::Utc::now().to_rfc3339();
    // The tuple is the `RETURNING` clause's own shape and lives only until the `map` below builds
    // `Claimed`; a named struct would repeat the column order in two places.
    #[allow(clippy::type_complexity)]
    let row: Option<(i64, Option<String>, String, String, i64, Option<i64>)> = sqlx::query_as(
        "UPDATE verify_runs SET status = 'running', started_at = ? \
         WHERE id = ? AND status = 'queued' \
         RETURNING id, project_id, worktree, argv, weight, timeout_ms",
    )
    .bind(started_at)
    .bind(id)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(
        |(id, project_id, worktree, argv, weight, timeout_ms)| Claimed {
            id,
            project_id,
            worktree,
            argv: serde_json::from_str(&argv).unwrap_or_default(),
            weight,
            timeout_ms: timeout_ms.unwrap_or(0),
        },
    ))
}

/// Records how a running row ended.
#[cfg_attr(not(test), allow(dead_code))]
pub async fn finish(
    pool: &SqlitePool,
    id: i64,
    status: &str,
    exit_code: Option<i64>,
    duration_ms: i64,
    output_tail: Option<&str>,
) -> sqlx::Result<()> {
    let finished_at = chrono::Utc::now().to_rfc3339();
    let output_tail = output_tail.map(tail);
    sqlx::query(
        "UPDATE verify_runs SET status = ?, exit_code = ?, duration_ms = ?, finished_at = ?, \
         output_tail = ? WHERE id = ? AND status = 'running'",
    )
    .bind(status)
    .bind(exit_code)
    .bind(duration_ms)
    .bind(finished_at)
    .bind(output_tail)
    .bind(id)
    .execute(pool)
    .await?;
    Ok(())
}

/// One row as a reader sees it.
#[cfg_attr(not(test), allow(dead_code))]
pub async fn get(pool: &SqlitePool, id: i64) -> sqlx::Result<Option<State>> {
    // The tuple is the `SELECT`'s own shape and lives only until the `map` below builds `State`;
    // a named struct would repeat the column order in two places.
    #[allow(clippy::type_complexity)]
    let row: Option<(i64, String, Option<i64>, Option<i64>, Option<String>, i64)> = sqlx::query_as(
        "SELECT id, status, exit_code, duration_ms, output_tail, interruptions \
             FROM verify_runs WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(
        |(id, status, exit_code, duration_ms, output_tail, interruptions)| State {
            id,
            status,
            exit_code,
            duration_ms,
            output_tail,
            interruptions,
        },
    ))
}

/// At daemon start, rows still `running` were cut off by the restart: they go back to the queue,
/// or are given up on once they have been interrupted `MAX_INTERRUPTIONS` times. Returns
/// `(requeued, given_up)`.
#[cfg_attr(not(test), allow(dead_code))]
pub async fn requeue_interrupted(pool: &SqlitePool) -> sqlx::Result<(u64, u64)> {
    let now = chrono::Utc::now().to_rfc3339();
    let given_up = sqlx::query(
        "UPDATE verify_runs SET status = 'errored', finished_at = ?, \
         output_tail = ?, interruptions = interruptions + 1 \
         WHERE status = 'running' AND interruptions + 1 >= ?",
    )
    .bind(now)
    .bind(format!(
        "interrupted by daemon restarts {MAX_INTERRUPTIONS} times"
    ))
    .bind(MAX_INTERRUPTIONS)
    .execute(pool)
    .await?
    .rows_affected();
    let requeued = sqlx::query(
        "UPDATE verify_runs SET status = 'queued', started_at = NULL, \
         interruptions = interruptions + 1 WHERE status = 'running'",
    )
    .execute(pool)
    .await?
    .rows_affected();
    Ok((requeued, given_up))
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
        crate::storage::MIGRATOR.run(&pool).await.unwrap();
        pool
    }

    fn ctx() -> GateContext<'static> {
        GateContext {
            project_id: Some("alpha"),
            origin: ORIGIN_RUN,
            origin_id: Some(7),
            ordinal: None,
        }
    }

    async fn only_row(
        pool: &SqlitePool,
    ) -> (
        String,
        Option<i64>,
        i64,
        String,
        Option<String>,
        Option<String>,
    ) {
        sqlx::query_as(
            "SELECT status, exit_code, duration_ms, argv, output_tail, sha FROM verify_runs",
        )
        .fetch_one(pool)
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn a_passing_gate_is_recorded_with_its_duration() {
        let pool = test_pool().await;
        let dir = tempfile::tempdir().unwrap();
        let outcome = timed_gate(
            Some(&pool),
            ctx(),
            dir.path(),
            dir.path(),
            r#"sh -c "exit 0""#,
            Duration::from_secs(30),
        )
        .await;
        assert!(matches!(outcome, GateOutcome::Passed));
        let (status, exit_code, duration_ms, argv, tail, _) = only_row(&pool).await;
        assert_eq!(status, STATUS_PASSED);
        assert_eq!(exit_code, Some(0));
        assert!(duration_ms >= 0);
        assert_eq!(argv, r#"sh -c "exit 0""#);
        assert_eq!(tail, None);
        let (origin, origin_id, scope, by): (String, Option<i64>, String, String) =
            sqlx::query_as("SELECT origin, origin_id, scope, requested_by FROM verify_runs")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!((origin.as_str(), origin_id), (ORIGIN_RUN, Some(7)));
        assert_eq!(
            (scope.as_str(), by.as_str()),
            (SCOPE_FULL, REQUESTED_BY_GATE)
        );
    }

    #[tokio::test]
    async fn a_failing_gate_records_its_exit_code_and_tail() {
        let pool = test_pool().await;
        let dir = tempfile::tempdir().unwrap();
        let outcome = timed_gate(
            Some(&pool),
            ctx(),
            dir.path(),
            dir.path(),
            r#"sh -c "echo boom; exit 3""#,
            Duration::from_secs(30),
        )
        .await;
        assert!(matches!(outcome, GateOutcome::Failed { exit_code: 3, .. }));
        let (status, exit_code, _, _, tail, _) = only_row(&pool).await;
        assert_eq!(status, STATUS_FAILED);
        assert_eq!(exit_code, Some(3));
        assert!(tail.unwrap().contains("boom"));
    }

    #[tokio::test]
    async fn an_errored_gate_is_recorded_as_errored() {
        let pool = test_pool().await;
        let dir = tempfile::tempdir().unwrap();
        let outcome = timed_gate(
            Some(&pool),
            ctx(),
            dir.path(),
            dir.path(),
            "nucleos-no-such-program-xyz",
            Duration::from_secs(5),
        )
        .await;
        assert!(matches!(outcome, GateOutcome::Errored { .. }));
        let (status, exit_code, _, _, tail, _) = only_row(&pool).await;
        assert_eq!(status, STATUS_ERRORED);
        assert_eq!(exit_code, None);
        assert!(
            tail.is_some(),
            "the reason is what makes an errored row readable"
        );
    }

    #[tokio::test]
    async fn without_a_pool_nothing_is_recorded_and_the_verdict_is_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let outcome = timed_gate(
            None,
            ctx(),
            dir.path(),
            dir.path(),
            r#"sh -c "exit 3""#,
            Duration::from_secs(30),
        )
        .await;
        assert!(matches!(outcome, GateOutcome::Failed { exit_code: 3, .. }));
    }

    #[tokio::test]
    async fn a_git_worktree_records_the_sha_it_measured() {
        let pool = test_pool().await;
        let dir = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            let ok = std::process::Command::new("git")
                .arg("-C")
                .arg(dir.path())
                .args(["-c", "user.name=t", "-c", "user.email=t@t"])
                .args(args)
                .status()
                .unwrap()
                .success();
            assert!(ok, "git {args:?}");
        };
        git(&["init", "-q"]);
        git(&["commit", "-q", "--allow-empty", "-m", "x"]);
        let head = String::from_utf8(
            std::process::Command::new("git")
                .arg("-C")
                .arg(dir.path())
                .args(["rev-parse", "HEAD"])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap();
        timed_gate(
            Some(&pool),
            ctx(),
            dir.path(),
            dir.path(),
            r#"sh -c "exit 0""#,
            Duration::from_secs(30),
        )
        .await;
        let (.., sha) = only_row(&pool).await;
        assert_eq!(sha.as_deref(), Some(head.trim()));
    }

    fn request(fingerprint: Option<&str>, priority: i64) -> Request {
        Request {
            project_id: Some("alpha".to_owned()),
            worktree: "/wt".to_owned(),
            scope: SCOPE_FULL.to_owned(),
            origin: ORIGIN_RUN.to_owned(),
            origin_id: Some(1),
            requested_by: "agent".to_owned(),
            group_name: Some("core".to_owned()),
            kind: Some("test".to_owned()),
            argv: vec!["cargo".to_owned(), "test".to_owned()],
            fingerprint: fingerprint.map(str::to_owned),
            priority,
            weight: 2,
            timeout_ms: 60_000,
        }
    }

    async fn row_count(pool: &SqlitePool) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM verify_runs")
            .fetch_one(pool)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn a_queued_request_is_claimed_once() {
        let pool = test_pool().await;
        let submitted = enqueue(&pool, &request(None, 1), 100).await.unwrap();
        assert!(!submitted.joined);
        let claimed = claim(&pool, submitted.id).await.unwrap().unwrap();
        assert_eq!(claimed.argv, vec!["cargo", "test"]);
        assert_eq!((claimed.weight, claimed.timeout_ms), (2, 60_000));
        assert!(claim(&pool, submitted.id).await.unwrap().is_none());
        let state = get(&pool, submitted.id).await.unwrap().unwrap();
        assert_eq!(state.status, STATUS_RUNNING);
    }

    #[tokio::test]
    async fn finishing_records_the_outcome() {
        let pool = test_pool().await;
        let id = enqueue(&pool, &request(None, 1), 100).await.unwrap().id;
        claim(&pool, id).await.unwrap().unwrap();
        finish(&pool, id, STATUS_PASSED, Some(0), 1234, Some("ok"))
            .await
            .unwrap();
        let state = get(&pool, id).await.unwrap().unwrap();
        assert_eq!(state.status, STATUS_PASSED);
        assert_eq!(state.exit_code, Some(0));
        assert_eq!(state.duration_ms, Some(1234));
        assert_eq!(state.output_tail.as_deref(), Some("ok"));
    }

    #[tokio::test]
    async fn an_equal_request_joins_the_one_in_flight() {
        let pool = test_pool().await;
        let first = enqueue(&pool, &request(Some("fp"), 1), 100).await.unwrap();
        let second = enqueue(&pool, &request(Some("fp"), 1), 200).await.unwrap();
        assert!(!first.joined);
        assert!(second.joined);
        assert_eq!(first.id, second.id);
        assert_eq!(row_count(&pool).await, 1);
    }

    #[tokio::test]
    async fn joining_raises_the_priority_of_a_queued_row() {
        let pool = test_pool().await;
        let first = enqueue(&pool, &request(Some("fp"), 1), 100).await.unwrap();
        enqueue(&pool, &request(Some("fp"), PRIORITY_INTERACTIVE), 200)
            .await
            .unwrap();
        let priority: i64 = sqlx::query_scalar("SELECT priority FROM verify_runs WHERE id = ?")
            .bind(first.id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(priority, PRIORITY_INTERACTIVE);
    }

    #[tokio::test]
    async fn a_request_without_a_fingerprint_never_joins() {
        let pool = test_pool().await;
        let first = enqueue(&pool, &request(None, 1), 100).await.unwrap();
        let second = enqueue(&pool, &request(None, 1), 200).await.unwrap();
        assert!(!second.joined);
        assert_ne!(first.id, second.id);
        assert_eq!(row_count(&pool).await, 2);
    }

    #[tokio::test]
    async fn a_different_fingerprint_does_not_join() {
        let pool = test_pool().await;
        let first = enqueue(&pool, &request(Some("a"), 1), 100).await.unwrap();
        let second = enqueue(&pool, &request(Some("b"), 1), 200).await.unwrap();
        assert!(!second.joined);
        assert_ne!(first.id, second.id);
    }

    #[tokio::test]
    async fn a_finished_row_is_not_joined() {
        let pool = test_pool().await;
        let first = enqueue(&pool, &request(Some("fp"), 1), 100).await.unwrap();
        claim(&pool, first.id).await.unwrap().unwrap();
        finish(&pool, first.id, STATUS_PASSED, Some(0), 5, None)
            .await
            .unwrap();
        let second = enqueue(&pool, &request(Some("fp"), 1), 200).await.unwrap();
        assert!(!second.joined);
        assert_ne!(first.id, second.id);
    }

    #[tokio::test]
    async fn a_restart_requeues_running_rows() {
        let pool = test_pool().await;
        let id = enqueue(&pool, &request(None, 1), 100).await.unwrap().id;
        claim(&pool, id).await.unwrap().unwrap();
        assert_eq!(requeue_interrupted(&pool).await.unwrap(), (1, 0));
        let state = get(&pool, id).await.unwrap().unwrap();
        assert_eq!(state.status, STATUS_QUEUED);
        assert_eq!(state.interruptions, 1);
        let started: Option<String> =
            sqlx::query_scalar("SELECT started_at FROM verify_runs WHERE id = ?")
                .bind(id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(started, None);
    }

    #[tokio::test]
    async fn a_row_interrupted_three_times_gives_up() {
        let pool = test_pool().await;
        let id = enqueue(&pool, &request(None, 1), 100).await.unwrap().id;
        for cycle in 1..=MAX_INTERRUPTIONS {
            claim(&pool, id).await.unwrap().unwrap();
            let counts = requeue_interrupted(&pool).await.unwrap();
            if cycle < MAX_INTERRUPTIONS {
                assert_eq!(counts, (1, 0));
            } else {
                assert_eq!(counts, (0, 1));
            }
        }
        let state = get(&pool, id).await.unwrap().unwrap();
        assert_eq!(state.status, STATUS_ERRORED);
        assert_eq!(
            state.output_tail.as_deref(),
            Some("interrupted by daemon restarts 3 times")
        );
    }

    #[tokio::test]
    async fn the_gate_log_is_not_in_the_queue() {
        let pool = test_pool().await;
        let row = Row {
            project_id: Some("alpha"),
            worktree: "/wt",
            sha: None,
            scope: SCOPE_FULL,
            origin: ORIGIN_RUN,
            origin_id: Some(1),
            ordinal: None,
            requested_by: REQUESTED_BY_GATE,
            group_name: None,
            kind: None,
            argv: "x",
            fingerprint: None,
            status: STATUS_PASSED,
            exit_code: Some(0),
            duration_ms: 1,
            started_at: "2026-01-01T00:00:00Z",
            finished_at: "2026-01-01T00:00:01Z",
            output_tail: None,
        };
        record(&pool, &row).await.unwrap();
        assert!(queued(&pool).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn the_rebuild_keeps_existing_rows() {
        let pool = crate::testdb::pool_migrated_through(166).await;
        sqlx::query(
            "INSERT INTO verify_runs (id, project_id, worktree, sha, scope, origin, origin_id, \
             ordinal, requested_by, argv, status, exit_code, duration_ms, started_at, finished_at, \
             output_tail) VALUES (7, 'alpha', '/wt', 'abc', 'full', 'run', 3, 2, 'gate', 'make', \
             'failed', 1, 99, '2026-01-01T00:00:00Z', '2026-01-01T00:00:01Z', 'boom')",
        )
        .execute(&pool)
        .await
        .unwrap();
        crate::testdb::apply_migrations_after(&pool, 166).await;
        let row: (
            String,
            String,
            Option<i64>,
            Option<i64>,
            String,
            String,
            i64,
            String,
        ) = sqlx::query_as(
            "SELECT status, argv, exit_code, duration_ms, started_at, finished_at, \
                 interruptions, output_tail FROM verify_runs WHERE id = 7",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(
            row,
            (
                "failed".to_owned(),
                "make".to_owned(),
                Some(1),
                Some(99),
                "2026-01-01T00:00:00Z".to_owned(),
                "2026-01-01T00:00:01Z".to_owned(),
                0,
                "boom".to_owned()
            )
        );
    }

    #[test]
    fn tail_keeps_the_end_and_respects_char_boundaries() {
        let long = format!("{}fim", "é".repeat(OUTPUT_TAIL_CHARS));
        let t = tail(&long);
        assert_eq!(t.chars().count(), OUTPUT_TAIL_CHARS);
        assert!(t.ends_with("fim"));
        assert_eq!(tail("curto"), "curto");
    }
}
