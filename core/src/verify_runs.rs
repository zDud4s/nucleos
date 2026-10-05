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

pub const SCOPE_FULL: &str = "full";
pub const REQUESTED_BY_GATE: &str = "gate";

pub const STATUS_PASSED: &str = "passed";
pub const STATUS_FAILED: &str = "failed";
pub const STATUS_ERRORED: &str = "errored";

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
    pub argv: &'a str,
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
         requested_by, argv, status, exit_code, duration_ms, started_at, finished_at, output_tail) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(row.project_id)
    .bind(row.worktree)
    .bind(row.sha)
    .bind(row.scope)
    .bind(row.origin)
    .bind(row.origin_id)
    .bind(row.ordinal)
    .bind(row.requested_by)
    .bind(row.argv)
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
        argv: command,
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
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
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

    #[test]
    fn tail_keeps_the_end_and_respects_char_boundaries() {
        let long = format!("{}fim", "é".repeat(OUTPUT_TAIL_CHARS));
        let t = tail(&long);
        assert_eq!(t.chars().count(), OUTPUT_TAIL_CHARS);
        assert!(t.ends_with("fim"));
        assert_eq!(tail("curto"), "curto");
    }
}
