//! Metric 1, the landing queue (spec analytics-pipeline §6.1 "1. Fila", §6.2), over
//! `vcs_requests` windowed by `created_at`; metric 2, conflicts, over the rows that ended in the
//! window; metric 3, what waits for a person now.
//!
//! `failure_reason` and `discarded` are free text — paths, command output, the files a resolution
//! dropped — and are read here only to choose a bucket. They never leave this module: the JSON
//! carries buckets and counts.

use serde::Serialize;
use sqlx::SqlitePool;

use super::stats::{self, Dist};
use crate::analytics::Window;
use crate::execution::vcs::{MERGE_KIND, OWNER_DECISION_SQL, WANTS_A_PERSON_SQL};
use crate::resolver::DISCARDED_UNKNOWN;

/// Both windows in one read: `[previous.from, window.to)`. `unexecutable` is the structural marker
/// `Outcome::Unexecutable` writes (migration 0208); the 8 KiB `output_tail` is never loaded, and a
/// NULL tail means nothing here (retention empties it after 30 days).
pub(crate) const LANDING_ROWS_SQL: &str = "SELECT status, op, origin, created_at, started_at,
        finished_at, unexecutable, failure_reason
   FROM vcs_requests
  WHERE created_at >= ?1 AND created_at < ?2 AND (?3 IS NULL OR project_id = ?3)
  ORDER BY id";

/// Metric 2's read, windowed by when a row stopped mattering (spec §6.1, 2): `settled_at` when the
/// person it waited for was let off, else `finished_at`. A row with neither is still open and is
/// not in it. Current window only: conflicts carry no previous figure.
pub(crate) const LANDING_SETTLED_SQL: &str = "SELECT status, op, origin, created_at, started_at,
        finished_at, unexecutable, failure_reason, settled_reason, discarded
   FROM vcs_requests
  WHERE COALESCE(settled_at, finished_at) >= ?1 AND COALESCE(settled_at, finished_at) < ?2
    AND (?3 IS NULL OR project_id = ?3)
  ORDER BY id";

/// Metric 3's read (spec §6.1, 3): the ticket module's own predicates for "wants a person",
/// interpolated and never copied, plus the merges a person has yet to approve. No window: it is
/// the queue now.
pub(crate) fn owner_blocked_sql() -> String {
    format!(
        "SELECT created_at FROM vcs_requests
          WHERE (({WANTS_A_PERSON_SQL}) OR {OWNER_DECISION_SQL} OR status = 'awaiting_approval')
            AND (?1 IS NULL OR project_id = ?1)"
    )
}

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
    /// Only `LANDING_SETTLED_SQL` selects these two.
    #[sqlx(default)]
    settled_reason: Option<String>,
    #[sqlx(default)]
    discarded: Option<String>,
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
    /// Filled by `read`, from reads of their own.
    pub conflicts: Option<Conflicts>,
    pub owner_blocked: Option<OwnerBlocked>,
}

/// What published resolutions left behind (`vcs_requests.discarded`, migration 0083).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct WorkLost {
    /// Worked out, and every line the incoming branch added was published.
    pub nothing: i64,
    /// Worked out, and something is missing. A row whose loss could not be worked out is neither.
    pub lost: i64,
}

/// Metric 2 over the rows that ended in the window.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Conflicts {
    /// Rows in the conflict bucket of §6.2 (escalated, or failed on git's CONFLICT).
    pub count: i64,
    /// `count` over the rows that ended in the window.
    pub rate: Option<f64>,
    /// Conflicts that stopped wanting a person without one: a resolution landed them
    /// (`resolved`) or their source reached the target (`merged`).
    pub auto_resolved: i64,
    pub work_lost: WorkLost,
}

/// Metric 3: requests waiting for a person now, aged from `created_at` (no column says when a
/// row entered its state).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OwnerBlocked {
    pub count: i64,
    pub oldest_age_s: Option<i64>,
    pub median_age_s: Option<i64>,
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

/// Settlement reasons that mean the conflict went away by itself (`vcs::tickets`).
const AUTO_SETTLED: [&str; 2] = ["resolved", "merged"];

/// PURE: metric 2 from the rows that ended in the window.
pub(crate) fn conflicts_block(rows: &[Row]) -> Conflicts {
    let conflicts: Vec<&Row> = rows
        .iter()
        .filter(|row| bucket(row) == Some(Bucket::Conflict))
        .collect();
    let auto_resolved = conflicts
        .iter()
        .filter(|row| {
            row.settled_reason
                .as_deref()
                .is_some_and(|reason| AUTO_SETTLED.contains(&reason))
        })
        .count() as i64;
    let mut work_lost = WorkLost::default();
    for discarded in rows.iter().filter_map(|row| row.discarded.as_deref()) {
        if discarded == "nothing" {
            work_lost.nothing += 1;
        } else if !discarded.starts_with(DISCARDED_UNKNOWN) {
            work_lost.lost += 1;
        }
    }
    Conflicts {
        count: conflicts.len() as i64,
        rate: stats::ratio(conflicts.len() as i64, rows.len() as i64),
        auto_resolved,
        work_lost,
    }
}

/// PURE: metric 3 from the waiting rows' `created_at`. A stamp that does not parse is counted and
/// not aged.
pub(crate) fn owner_blocked_block(
    created: &[String],
    now: chrono::DateTime<chrono::Utc>,
) -> OwnerBlocked {
    let ages: Vec<i64> = created
        .iter()
        .filter_map(|at| stats::parse_at(at.as_str()))
        .map(|at| (now - at).num_seconds().max(0))
        .collect();
    let oldest_age_s = ages.iter().copied().max();
    OwnerBlocked {
        count: created.len() as i64,
        oldest_age_s,
        median_age_s: stats::dist(ages).p50,
    }
}

pub(crate) async fn read(
    pool: &SqlitePool,
    project: Option<&str>,
    window: Window,
    now: chrono::DateTime<chrono::Utc>,
) -> sqlx::Result<Landing> {
    let rows = rows(pool, project, window).await?;
    let mut landing = landing_block(&rows, window);
    let ended: Vec<Row> = sqlx::query_as(LANDING_SETTLED_SQL)
        .bind(window.from_bound())
        .bind(window.to_bound())
        .bind(project)
        .fetch_all(pool)
        .await?;
    landing.conflicts = Some(conflicts_block(&ended));
    let waiting: Vec<String> = sqlx::query_scalar(sqlx::AssertSqlSafe(owner_blocked_sql()))
        .bind(project)
        .fetch_all(pool)
        .await?;
    landing.owner_blocked = Some(owner_blocked_block(&waiting, now));
    Ok(landing)
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

    /// What `vcs::settle` writes when a row stops wanting a person.
    async fn settle(pool: &SqlitePool, id: i64, at: &str, reason: &str) {
        sqlx::query("UPDATE vcs_requests SET settled_at = ?, settled_reason = ? WHERE id = ?")
            .bind(at)
            .bind(reason)
            .bind(id)
            .execute(pool)
            .await
            .unwrap();
    }

    /// What the resolver writes once it has worked out what a published resolution dropped.
    async fn discard(pool: &SqlitePool, id: i64, what: &str) {
        sqlx::query("UPDATE vcs_requests SET from_resolution = 1, discarded = ? WHERE id = ?")
            .bind(what)
            .bind(id)
            .execute(pool)
            .await
            .unwrap();
    }

    /// Spec §6.1, 2: a conflict is an escalation or a failure on git's CONFLICT; it resolved
    /// itself when settled as `resolved` or `merged`; work lost splits `nothing` from a list of
    /// what is missing. The window is `settled_at`, else `finished_at`.
    #[tokio::test]
    async fn conflicts_count_escalated_and_conflict_failures_auto_resolved_and_work_lost() {
        let pool = crate::testdb::fresh_pool().await;
        let long_ago = Req {
            created: "2026-10-01T10:00:00+00:00",
            started: Some("2026-10-01T10:00:12+00:00"),
            finished: Some("2026-10-01T10:00:53+00:00"),
            ..ended("escalated", "conflict")
        };
        let today = "2026-10-17T11:00:00+00:00";
        // Finished before the window, settled inside it: in.
        let id = request(&pool, long_ago.clone()).await;
        settle(&pool, id, "2026-10-16T10:00:00+00:00", "resolved").await;
        let conflict_text = "CONFLICT (content): Merge conflict in a.rs";
        request(&pool, ended("failed", conflict_text)).await;
        let id = request(&pool, ended("escalated", "x")).await;
        settle(&pool, id, today, "merged").await;
        let id = request(&pool, ended("escalated", "x")).await;
        settle(&pool, id, today, "dismissed").await;
        // Settled by itself, but blocked is not a conflict.
        let id = request(&pool, ended("blocked", "x")).await;
        settle(&pool, id, today, "resolved").await;
        let unknown = format!("{DISCARDED_UNKNOWN}timed out");
        for what in [
            "nothing",
            "a.rs: 4 lines of the incoming side",
            unknown.as_str(),
        ] {
            let id = request(&pool, merged()).await;
            discard(&pool, id, what).await;
        }
        // Settled before the window: out. Still queued, with neither stamp: out.
        let id = request(&pool, long_ago).await;
        settle(&pool, id, "2026-10-05T10:00:00+00:00", "resolved").await;
        let queued = Req {
            status: "queued",
            started: None,
            finished: None,
            ..merged()
        };
        request(&pool, queued).await;

        let conflicts = block_of(&pool).await.conflicts.unwrap();
        assert_eq!(conflicts.count, 4, "{conflicts:?}");
        assert_eq!(conflicts.rate, Some(0.5), "four of eight ended rows");
        assert_eq!(conflicts.auto_resolved, 2, "not the dismissed one");
        let lost = WorkLost {
            nothing: 1,
            lost: 1,
        };
        assert_eq!(conflicts.work_lost, lost, "an unknown loss is neither");
        let json = serde_json::to_string(&conflicts).unwrap();
        assert!(!json.contains("a.rs"), "{json}");
    }

    /// Spec §6.1, 3: what the Waiting page counts (`WANTS_A_PERSON_SQL`, `OWNER_DECISION_SQL`,
    /// interpolated) plus `awaiting_approval`, with no window, aged from `created_at` to now.
    #[tokio::test]
    async fn owner_blocked_reuses_the_ticket_predicates_and_ages_from_created_at() {
        let sql = owner_blocked_sql();
        assert!(sql.contains(WANTS_A_PERSON_SQL));
        assert!(sql.contains(OWNER_DECISION_SQL));
        let pool = crate::testdb::fresh_pool().await;
        let waiting = |status: &'static str, created: &'static str| Req {
            status,
            created,
            started: None,
            finished: None,
            ..merged()
        };
        for (status, created) in [
            ("escalated", "2026-10-17T13:03:11+00:00"),      // 3600 s
            ("blocked", "2026-10-17T14:02:11+00:00"),        // 60 s
            ("awaiting_owner", "2026-10-17T13:53:11+00:00"), // 600 s
            // Long before any window, and still waiting: 77 days and 14:03:11.
            ("awaiting_owner", "2026-08-01T00:00:00+00:00"),
        ] {
            request(&pool, waiting(status, created)).await;
        }
        // 7200 s, in another project.
        let approval = Req {
            project: "q",
            ..waiting("awaiting_approval", "2026-10-17T12:03:11+00:00")
        };
        request(&pool, approval).await;
        // Settled, queued and failed rows want nobody.
        let settled = request(&pool, waiting("escalated", "2026-10-17T10:00:00+00:00")).await;
        settle(&pool, settled, "2026-10-17T11:00:00+00:00", "dismissed").await;
        request(&pool, waiting("queued", "2026-10-17T10:00:00+00:00")).await;
        request(&pool, ended("failed", "x")).await;

        let global = block_of(&pool).await.owner_blocked.unwrap();
        let badge = crate::execution::vcs::count_waiting_on_a_person(&pool)
            .await
            .unwrap() as i64;
        assert_eq!(global.count, badge + 1, "the badge plus the approval");
        let oldest = 77 * 86_400 + 14 * 3600 + 3 * 60 + 11;
        let expected = OwnerBlocked {
            count: 5,
            oldest_age_s: Some(oldest),
            median_age_s: Some(3600),
        };
        assert_eq!(global, expected);

        let window = Window::new(7, now()).unwrap();
        let project = read(&pool, Some("p"), window, now()).await.unwrap();
        let expected = OwnerBlocked {
            count: 4,
            oldest_age_s: Some(oldest),
            median_age_s: Some(600),
        };
        assert_eq!(project.owner_blocked, Some(expected), "not q's approval");
    }
}
