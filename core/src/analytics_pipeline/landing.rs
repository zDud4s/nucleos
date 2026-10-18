//! Metric 1, the landing queue (spec analytics-pipeline §6.1 "1. Fila", §6.2), over
//! `vcs_requests` windowed by `created_at`. Metrics 2 and 3 join this block in the follow-up
//! (`conflicts`, `owner_blocked`, `null` until then).
//!
//! `failure_reason` is free text — paths, command output — and is read here only to choose a
//! bucket. It never leaves this module: the JSON carries buckets and counts.

use serde::Serialize;
use sqlx::SqlitePool;

use super::stats::{self, Dist};
use crate::analytics::Window;
use crate::execution::vcs::MERGE_KIND;

/// Both windows in one read: `[previous.from, window.to)`. `unexecutable` is the structural marker
/// `Outcome::Unexecutable` writes (migration 0208); the 8 KiB `output_tail` is never loaded, and a
/// NULL tail means nothing here (retention empties it after 30 days).
pub(crate) const LANDING_ROWS_SQL: &str = "SELECT status, op, origin, created_at, started_at,
        finished_at, unexecutable, failure_reason
   FROM vcs_requests
  WHERE created_at >= ?1 AND created_at < ?2 AND (?3 IS NULL OR project_id = ?3)
  ORDER BY id";

#[derive(Debug, Clone, sqlx::FromRow)]
pub(crate) struct Row {
    pub status: String,
    pub op: String,
    pub origin: String,
    pub created_at: String,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub unexecutable: bool,
    failure_reason: Option<String>,
}

pub(crate) async fn rows(
    pool: &SqlitePool,
    project: Option<&str>,
    window: Window,
) -> sqlx::Result<Vec<Row>> {
    sqlx::query_as(LANDING_ROWS_SQL)
        .bind(window.previous().from_bound())
        .bind(window.to_bound())
        .bind(project)
        .fetch_all(pool)
        .await
}

/// §6.2's buckets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Bucket {
    Conflict,
    Unexecutable,
    Blocked,
    Gate,
    Interrupted,
    Other,
}

/// §6.2's `LIKE` patterns, lower-cased and matched case-insensitively, as SQLite's `LIKE` matches
/// ASCII. Pinned to real messages by
/// `failure_like_patterns_match_the_messages_the_queue_really_writes`.
pub(crate) const CONFLICT_MARK: &str = "conflict";
pub(crate) const GATE_MARK: &str = "gate";

fn reason_has(row: &Row, mark: &str) -> bool {
    row.failure_reason
        .as_deref()
        .is_some_and(|reason| reason.to_ascii_lowercase().contains(mark))
}

/// A failed row carrying the marker. Rows written before migration 0208 default to 0 and are not
/// unexecutable: they fall to `other` (owner decision 1, no backfill).
pub(crate) fn is_unexecutable(row: &Row) -> bool {
    row.status == "failed" && row.unexecutable
}

/// The first §6.2 rule that matches; `None` for a row that did not go wrong.
pub(crate) fn bucket(row: &Row) -> Option<Bucket> {
    match row.status.as_str() {
        "escalated" => Some(Bucket::Conflict),
        "failed" if is_unexecutable(row) => Some(Bucket::Unexecutable),
        "failed" if reason_has(row, CONFLICT_MARK) => Some(Bucket::Conflict),
        "blocked" => Some(Bucket::Blocked),
        "failed" if row.op == MERGE_KIND && reason_has(row, GATE_MARK) => Some(Bucket::Gate),
        "interrupted" => Some(Bucket::Interrupted),
        "failed" => Some(Bucket::Other),
        _ => None,
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct StatusMix {
    pub succeeded: i64,
    pub failed: i64,
    pub escalated: i64,
    pub blocked: i64,
    pub cancelled: i64,
    /// queued, running, awaiting approval or owner, rejected, interrupted.
    pub other: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct FailureBuckets {
    pub conflict: i64,
    pub gate: i64,
    pub blocked: i64,
    pub interrupted: i64,
    pub other: i64,
}

/// Requests created that day, and how many of them went wrong (escalated, blocked or a real
/// failure). One point per window day; `partial` on today.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LandingDay {
    pub day: String,
    pub partial: bool,
    pub total: i64,
    pub bad: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Landing {
    pub total: i64,
    pub prev_total: i64,
    pub wait_s: Dist,
    pub wait_s_machine: Dist,
    pub run_s: Dist,
    pub status_mix: StatusMix,
    pub unexecutable: i64,
    pub bad_share: Option<f64>,
    pub prev_bad_share: Option<f64>,
    pub failure_buckets: FailureBuckets,
    pub daily: Vec<LandingDay>,
    /// Metric 2 (follow-up). `null` until then.
    pub conflicts: Option<serde_json::Value>,
    /// Metric 3 (follow-up). `null` until then.
    pub owner_blocked: Option<serde_json::Value>,
}

fn in_window(row: &Row, window: Window) -> bool {
    row.created_at
        .get(..10)
        .is_some_and(|day| window.holds(day))
}

/// Escalated, blocked or a failure of the work itself (not of the row).
fn is_bad(row: &Row) -> bool {
    match row.status.as_str() {
        "escalated" | "blocked" => true,
        "failed" => !is_unexecutable(row),
        _ => false,
    }
}

fn bad_share(rows: &[&Row]) -> Option<f64> {
    let bad = rows.iter().filter(|row| is_bad(row)).count() as i64;
    stats::ratio(bad, rows.len() as i64)
}

/// Seconds a request waited: `created_at` to `started_at`.
fn wait_secs(row: &Row) -> Option<i64> {
    stats::secs_between(&row.created_at, row.started_at.as_deref()?)
}

/// Seconds a request ran: `started_at` to `finished_at`.
fn run_secs(row: &Row) -> Option<i64> {
    stats::secs_between(row.started_at.as_deref()?, row.finished_at.as_deref()?)
}

fn within(rows: &[Row], window: Window) -> Vec<&Row> {
    rows.iter().filter(|row| in_window(row, window)).collect()
}

/// PURE: the block from the rows of both windows.
pub(crate) fn landing_block(rows: &[Row], window: Window) -> Landing {
    let now_rows = within(rows, window);
    let prev_rows = within(rows, window.previous());

    let wait = |machine_only: bool| {
        let values = now_rows
            .iter()
            .copied()
            .filter(|row| !machine_only || row.origin != "human")
            .filter_map(wait_secs)
            .collect();
        stats::dist(values)
    };
    let run_s = stats::dist(now_rows.iter().copied().filter_map(run_secs).collect());

    let mut mix = StatusMix::default();
    let mut buckets = FailureBuckets::default();
    let mut unexecutable = 0;
    for row in &now_rows {
        match row.status.as_str() {
            "succeeded" => mix.succeeded += 1,
            "failed" => mix.failed += 1,
            "escalated" => mix.escalated += 1,
            "blocked" => mix.blocked += 1,
            "cancelled" => mix.cancelled += 1,
            _ => mix.other += 1,
        }
        match bucket(row) {
            Some(Bucket::Conflict) => buckets.conflict += 1,
            Some(Bucket::Unexecutable) => unexecutable += 1,
            Some(Bucket::Blocked) => buckets.blocked += 1,
            Some(Bucket::Gate) => buckets.gate += 1,
            Some(Bucket::Interrupted) => buckets.interrupted += 1,
            Some(Bucket::Other) => buckets.other += 1,
            None => {}
        }
    }

    let today = window.span().to;
    let daily = window
        .day_list()
        .into_iter()
        .map(|day| {
            let of_day: Vec<&Row> = now_rows
                .iter()
                .copied()
                .filter(|row| row.created_at.starts_with(&day))
                .collect();
            LandingDay {
                partial: day == today,
                total: of_day.len() as i64,
                bad: of_day.iter().filter(|row| is_bad(row)).count() as i64,
                day,
            }
        })
        .collect();

    Landing {
        total: now_rows.len() as i64,
        prev_total: prev_rows.len() as i64,
        wait_s: wait(false),
        wait_s_machine: wait(true),
        run_s,
        status_mix: mix,
        unexecutable,
        bad_share: bad_share(&now_rows),
        prev_bad_share: bad_share(&prev_rows),
        failure_buckets: buckets,
        daily,
        conflicts: None,
        owner_blocked: None,
    }
}

pub(crate) async fn read(
    pool: &SqlitePool,
    project: Option<&str>,
    window: Window,
    // Unused since owner decision 1 (unexecutable is a column, no retention cut-off to compute);
    // kept so every block's `read` has the same shape for Task 3.3.
    _now: chrono::DateTime<chrono::Utc>,
) -> sqlx::Result<Landing> {
    let rows = rows(pool, project, window).await?;
    Ok(landing_block(&rows, window))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::analytics::tests::now;

    #[derive(Clone)]
    pub(crate) struct Req {
        pub(crate) project: &'static str,
        pub(crate) op: &'static str,
        pub(crate) origin: &'static str,
        pub(crate) status: &'static str,
        pub(crate) created: &'static str,
        pub(crate) started: Option<&'static str>,
        pub(crate) finished: Option<&'static str>,
        pub(crate) tail: Option<&'static str>,
        pub(crate) reason: Option<&'static str>,
        /// `vcs_requests.unexecutable`, the marker `Outcome::Unexecutable` writes (Task 0.2).
        pub(crate) unexecutable: bool,
    }

    /// A merge a run asked for, succeeded on the clock's day with a 12 s wait and a 41 s run.
    pub(crate) fn merged() -> Req {
        Req {
            project: "p",
            op: "merge",
            origin: "run",
            status: "succeeded",
            created: "2026-10-17T10:00:00+00:00",
            started: Some("2026-10-17T10:00:12+00:00"),
            finished: Some("2026-10-17T10:00:53+00:00"),
            tail: Some(""),
            reason: None,
            unexecutable: false,
        }
    }

    /// A `merged()` that ended in `status` for `reason`.
    fn ended(status: &'static str, reason: &'static str) -> Req {
        Req {
            status,
            reason: Some(reason),
            ..merged()
        }
    }

    /// A failed row the way `Outcome::Unexecutable` leaves it: marked, with no output tail.
    fn marked(reason: &'static str) -> Req {
        Req {
            tail: None,
            unexecutable: true,
            ..ended("failed", reason)
        }
    }

    pub(crate) async fn request(pool: &SqlitePool, r: Req) -> i64 {
        sqlx::query_scalar(
            "INSERT INTO vcs_requests (op, args, project_id, project_root, origin, status,
                                       output_tail, failure_reason, created_at, started_at,
                                       finished_at, unexecutable)
             VALUES (?, '{}', ?, '/r', ?, ?, ?, ?, ?, ?, ?, ?) RETURNING id",
        )
        .bind(r.op)
        .bind(r.project)
        .bind(r.origin)
        .bind(r.status)
        .bind(r.tail)
        .bind(r.reason)
        .bind(r.created)
        .bind(r.started)
        .bind(r.finished)
        .bind(r.unexecutable)
        .fetch_one(pool)
        .await
        .unwrap()
    }

    async fn block_of(pool: &SqlitePool) -> Landing {
        let window = Window::new(7, now()).unwrap();
        read(pool, None, window, now()).await.unwrap()
    }

    /// Spec §11: `wait_s` needs `started_at`, `run_s` needs both ends; a human's request counts
    /// in `wait_s` and not in `wait_s_machine`.
    #[tokio::test]
    async fn landing_wait_needs_started_at_and_run_needs_finished_at() {
        let pool = crate::testdb::fresh_pool().await;
        request(&pool, merged()).await; // wait 12, run 41
        let queued = Req {
            status: "queued",
            started: None,
            finished: None,
            ..merged()
        };
        request(&pool, queued).await;
        let awaiting = Req {
            status: "awaiting_owner",
            started: Some("2026-10-17T10:00:30+00:00"), // wait 30
            finished: None,
            ..merged()
        };
        request(&pool, awaiting).await;
        let human = Req {
            origin: "human",
            status: "running",
            started: Some("2026-10-17T10:05:00+00:00"), // wait 300
            finished: None,
            ..merged()
        };
        request(&pool, human).await;

        let block = block_of(&pool).await;
        assert_eq!(block.total, 4);
        let wait = Dist {
            p50: Some(30),
            p95: None,
            n: 3,
        };
        assert_eq!(block.wait_s, wait);
        let machine = Dist {
            p50: Some(12),
            p95: None,
            n: 2,
        };
        assert_eq!(block.wait_s_machine, machine);
        let run = Dist {
            p50: Some(41),
            p95: None,
            n: 1,
        };
        assert_eq!(block.run_s, run);
    }

    /// Spec §11: an unexecutable row (failed, `unexecutable = 1`) is counted apart, not as bad
    /// work.
    #[tokio::test]
    async fn landing_bad_share_excludes_unexecutable_from_failed() {
        let pool = crate::testdb::fresh_pool().await;
        for _ in 0..6 {
            request(&pool, merged()).await;
        }
        request(&pool, ended("failed", "nope")).await;
        request(&pool, marked("bad row")).await;
        request(&pool, ended("escalated", "gate")).await;
        request(&pool, ended("blocked", "gate")).await;

        let block = block_of(&pool).await;
        assert_eq!(block.total, 10);
        assert_eq!(block.unexecutable, 1);
        assert_eq!(
            block.bad_share,
            Some(0.3),
            "escalated + blocked + one real failure, over ten"
        );
        assert_eq!(block.prev_total, 0);
        assert_eq!(block.prev_bad_share, None);
        assert_eq!(block.status_mix.failed, 2);
        assert_eq!(block.status_mix.succeeded, 6);
    }

    /// Owner decision 1: unexecutable is the `vcs_requests.unexecutable` marker (migration 0208,
    /// Task 0.2), never a NULL tail. `prune_output_tails` empties the tail of every finished row
    /// after 30 days, so a failed row with no tail and no marker (a row from before the migration,
    /// or an ordinary failure whose tail was pruned) is NOT unexecutable and is bad work; a marked
    /// row older than 30 days with its tail pruned still IS unexecutable.
    #[tokio::test]
    async fn a_null_tail_without_the_marker_is_not_unexecutable_but_a_marked_row_is() {
        let pool = crate::testdb::fresh_pool().await;
        let old = Req {
            status: "failed",
            tail: None,
            created: "2026-08-20T10:00:00+00:00",
            started: Some("2026-08-20T10:00:01+00:00"),
            finished: Some("2026-08-20T10:00:02+00:00"),
            ..merged()
        };
        let unmarked = Req {
            reason: Some("pre-migration or pruned"),
            ..old.clone()
        };
        request(&pool, unmarked).await;
        let marked = Req {
            unexecutable: true,
            reason: Some("bad row"),
            ..old
        };
        request(&pool, marked).await;

        let window = Window::new(90, now()).unwrap();
        let block = read(&pool, None, window, now()).await.unwrap();
        assert_eq!(block.unexecutable, 1, "only the marked row");
        assert_eq!(
            block.failure_buckets.other, 1,
            "the unmarked NULL-tail failure falls to other"
        );
        assert_eq!(
            block.bad_share,
            Some(0.5),
            "the unmarked failure is bad work; the marked one is not"
        );
    }

    /// Spec §11 and §6.2: status first, then op, then the text; one row per bucket.
    #[tokio::test]
    async fn failure_bucket_follows_status_then_op_then_like() {
        let pool = crate::testdb::fresh_pool().await;
        let gate_text = "the merge does not pass this project's gate";
        let push = Req {
            op: "push",
            ..ended("failed", "the gate said no")
        };
        let cases = [
            (ended("escalated", "gate"), Some(Bucket::Conflict)),
            (marked("CONFLICT"), Some(Bucket::Unexecutable)),
            (
                ended("failed", "CONFLICT (content): Merge conflict in a.rs"),
                Some(Bucket::Conflict),
            ),
            (ended("blocked", "gate"), Some(Bucket::Blocked)),
            (ended("failed", gate_text), Some(Bucket::Gate)),
            (push, Some(Bucket::Other)),
            (ended("interrupted", "restart"), Some(Bucket::Interrupted)),
            (ended("failed", "anything"), Some(Bucket::Other)),
            (merged(), None),
        ];
        let expected: Vec<Option<Bucket>> = cases.iter().map(|(_, want)| *want).collect();
        for (req, _) in cases {
            request(&pool, req).await;
        }
        let window = Window::new(7, now()).unwrap();
        let got: Vec<Option<Bucket>> = rows(&pool, None, window)
            .await
            .unwrap()
            .iter()
            .map(bucket)
            .collect();
        assert_eq!(got, expected, "rows come back in insertion order (id)");
    }

    /// Spec §11: the patterns are pinned to text the queue really writes, so a reworded message
    /// fails here instead of silently moving its rows to `other`. Conflicts on a merge normally
    /// arrive as `escalated` (rule 1); rule 3 catches git's own CONFLICT line on other ops.
    #[tokio::test]
    async fn failure_like_patterns_match_the_messages_the_queue_really_writes() {
        use crate::execution::git_exec::merge_gate_failed;
        use crate::execution::vcs::{Outcome, finish};
        let pool = crate::testdb::fresh_pool().await;
        let running = Req {
            status: "running",
            finished: None,
            ..merged()
        };
        let gate = request(&pool, running.clone()).await;
        finish(&pool, gate, merge_gate_failed(1, "red".into()))
            .await
            .unwrap();
        let rebase_req = Req {
            op: "rebase",
            ..running
        };
        let rebase = request(&pool, rebase_req).await;
        let conflict = Outcome::Failed {
            reason: "CONFLICT (content): Merge conflict in a.rs".into(),
            exit_code: Some(1),
            output_tail: "CONFLICT (content): Merge conflict in a.rs\n".into(),
        };
        finish(&pool, rebase, conflict).await.unwrap();

        let block = block_of(&pool).await;
        let buckets = &block.failure_buckets;
        assert_eq!(buckets.gate, 1, "{buckets:?}");
        assert_eq!(buckets.conflict, 1, "{buckets:?}");
    }

    /// Spec §11 and §9: the reason is read to bucket a row and never serialised.
    #[tokio::test]
    async fn the_failure_reason_text_never_reaches_the_json() {
        let pool = crate::testdb::fresh_pool().await;
        let leaky = ended("failed", "secret at /home/owner/.ssh: CONFLICT");
        request(&pool, leaky).await;
        let json = serde_json::to_string(&block_of(&pool).await).unwrap();
        assert!(!json.contains("secret") && !json.contains(".ssh"), "{json}");
    }
}
