//! Metric 8, the jobs (spec analytics-pipeline §6.1 "8. Jobs"): outcomes over the window by
//! `created_at`, and the stuck signals over every job that is live NOW. The thresholds are the job
//! module's own constants, imported rather than copied, so the screen follows the code.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::SqlitePool;

use super::stats::{self, Dist};
use crate::analytics::Window;
use crate::execution::job::{
    COMMISSIONED_JOB_LIFETIME, DRY_ROUNDS_TO_STOP, ItemState, MAX_JOB_LIFETIME, STATUS_CANCELLED,
    STATUS_EXPIRED, STATUS_INTERRUPTED, STATUS_STOPPED, commissioned_with_an_allowance,
    item_state_from,
};

/// Both windows in one read: `[previous.from, window.to)`.
pub(crate) const JOB_ROWS_SQL: &str =
    "SELECT id, project_id, status, created_at, completed_at FROM jobs
  WHERE created_at >= ?1 AND created_at < ?2 AND (?3 IS NULL OR project_id = ?3)";
/// `job::LIVE_STATUSES` spelled out (sqlx refuses SQL built at run time);
/// `the_live_list_here_is_job_live_statuses` holds the two together.
#[cfg(test)]
const LIVE_SQL_LIST: &str =
    "('planning','implementing','gating','reviewing','awaiting_approval','waiting')";
pub(crate) const LIVE_JOBS_SQL: &str =
    "SELECT id, project_id, rule_name, budget_usd, status, wait_reason,
        round, max_rounds, dry_rounds, gate_retries, created_at FROM jobs
  WHERE status IN ('planning','implementing','gating','reviewing','awaiting_approval','waiting')
    AND (?1 IS NULL OR project_id = ?1) ORDER BY id";
pub(crate) const LIVE_ITEMS_SQL: &str =
    "SELECT ji.job_id, ji.status, ji.gate_attempts FROM job_items ji
  JOIN jobs j ON j.id = ji.job_id
  WHERE j.status IN ('planning','implementing','gating','reviewing','awaiting_approval','waiting')
    AND (?1 IS NULL OR j.project_id = ?1)";

/// At most this many ids in `stuck.jobs`.
const STUCK_SHOWN: usize = 10;
/// Job endings that are failures of the work; the interrupted ones are `job::STATUS_*`.
const FAILED_STATUSES: [&str; 3] = ["failed", "gate_failed", "gate_errored"];
const INTERRUPTED_STATUSES: [&str; 4] = [
    STATUS_EXPIRED,
    STATUS_STOPPED,
    STATUS_INTERRUPTED,
    STATUS_CANCELLED,
];

#[derive(Debug, Clone, sqlx::FromRow)]
struct Row {
    status: String,
    created_at: String,
    completed_at: Option<String>,
}

#[derive(Debug, Clone, sqlx::FromRow)]
struct LiveJob {
    id: i64,
    project_id: String,
    rule_name: Option<String>,
    budget_usd: Option<f64>,
    status: String,
    wait_reason: Option<String>,
    round: i64,
    max_rounds: Option<i64>,
    dry_rounds: i64,
    gate_retries: i64,
    created_at: String,
}

/// Why waiting jobs wait. Every known reason is always present (0 when none); a reason this list
/// does not know, and a NULL one, count under `other`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Waiting {
    pub slot: i64,
    pub budget: i64,
    pub quota: i64,
    pub disk: i64,
    pub excluded: i64,
    #[serde(rename = "kill-switch")]
    pub kill_switch: i64,
    pub attention: i64,
    pub other: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StuckJob {
    pub id: i64,
    pub project_id: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Stuck {
    pub age_over_lifetime: i64,
    pub dry_rounds_at_limit: i64,
    pub rounds_exhausted: i64,
    pub gate_retry_loop: i64,
    pub item_stuck: i64,
    pub waiting: Waiting,
    pub jobs: Vec<StuckJob>,
}

/// Jobs created that day, and how many of them completed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct JobDay {
    pub day: String,
    pub partial: bool,
    pub created: i64,
    pub completed: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Jobs {
    pub total: i64,
    pub completed: i64,
    pub failed: i64,
    pub interrupted: i64,
    pub live: i64,
    pub completion_rate: Option<f64>,
    pub prev_completion_rate: Option<f64>,
    pub duration_s: Dist,
    pub daily: Vec<JobDay>,
    pub stuck: Stuck,
}

fn in_window(row: &Row, window: Window) -> bool {
    row.created_at
        .get(..10)
        .is_some_and(|day| window.holds(day))
}

fn is_completed(row: &Row) -> bool {
    row.status == "completed"
}

fn is_failed(row: &Row) -> bool {
    FAILED_STATUSES.contains(&row.status.as_str())
}

fn is_interrupted(row: &Row) -> bool {
    INTERRUPTED_STATUSES.contains(&row.status.as_str())
}

fn count(rows: &[&Row], keep: fn(&Row) -> bool) -> i64 {
    rows.iter().filter(|row| keep(row)).count() as i64
}

/// `completed / (completed + failed + interrupted)`: the jobs that ended.
fn completion_rate(rows: &[&Row]) -> Option<f64> {
    let completed = count(rows, is_completed);
    let ended = completed + count(rows, is_failed) + count(rows, is_interrupted);
    stats::ratio(completed, ended)
}

/// PURE: the stuck signals over the jobs live now, each counted once per job.
fn stuck_block(jobs: &[LiveJob], items: &[(i64, String, i64)], now: DateTime<Utc>) -> Stuck {
    let mut by_job: HashMap<i64, Vec<(&str, i64)>> = HashMap::new();
    for (job_id, status, attempts) in items {
        by_job
            .entry(*job_id)
            .or_default()
            .push((status.as_str(), *attempts));
    }

    let mut stuck = Stuck::default();
    for job in jobs {
        let lifetime = if commissioned_with_an_allowance(job.rule_name.as_deref(), job.budget_usd) {
            COMMISSIONED_JOB_LIFETIME
        } else {
            MAX_JOB_LIFETIME
        };
        let age = stats::parse_at(&job.created_at).is_some_and(|created| now - created > lifetime);
        let dry = job.dry_rounds >= DRY_ROUNDS_TO_STOP;
        let exhausted = job.max_rounds.is_some_and(|max| job.round >= max);

        let states: Vec<(i64, ItemState)> = by_job
            .get(&job.id)
            .into_iter()
            .flatten()
            .map(|(status, attempts)| {
                (
                    *attempts,
                    item_state_from(status, *attempts, job.gate_retries),
                )
            })
            .collect();
        // On its last allowed retry: `gate_retries` is the limit `start` copied from the rule's
        // config, so the limit is read from the config and not fixed here.
        let retry_loop = states.iter().any(|(attempts, state)| {
            *state == ItemState::GateRetriable
                && job.gate_retries > 0
                && *attempts >= job.gate_retries
        });
        let item_stuck = states
            .iter()
            .any(|(_, state)| matches!(state, ItemState::Conflicted | ItemState::GateFailed));
        let waiting = job.status == "waiting";

        stuck.age_over_lifetime += i64::from(age);
        stuck.dry_rounds_at_limit += i64::from(dry);
        stuck.rounds_exhausted += i64::from(exhausted);
        stuck.gate_retry_loop += i64::from(retry_loop);
        stuck.item_stuck += i64::from(item_stuck);
        if waiting {
            let reasons = &mut stuck.waiting;
            match job.wait_reason.as_deref() {
                Some("slot") => reasons.slot += 1,
                Some("budget") => reasons.budget += 1,
                Some("quota") => reasons.quota += 1,
                Some("disk") => reasons.disk += 1,
                Some("excluded") => reasons.excluded += 1,
                Some("kill-switch") => reasons.kill_switch += 1,
                Some("attention") => reasons.attention += 1,
                _ => reasons.other += 1,
            }
        }
        let flagged = age || dry || exhausted || retry_loop || item_stuck || waiting;
        if flagged && stuck.jobs.len() < STUCK_SHOWN {
            stuck.jobs.push(StuckJob {
                id: job.id,
                project_id: job.project_id.clone(),
            });
        }
    }
    stuck
}

/// PURE: the block from the rows of both windows and the live jobs.
fn jobs_block(rows: &[Row], stuck: Stuck, window: Window) -> Jobs {
    let now_rows: Vec<&Row> = rows.iter().filter(|r| in_window(r, window)).collect();
    let prev_rows: Vec<&Row> = rows
        .iter()
        .filter(|r| in_window(r, window.previous()))
        .collect();

    let durations = now_rows
        .iter()
        .filter(|row| is_completed(row) || is_failed(row) || is_interrupted(row))
        .filter_map(|row| stats::secs_between(&row.created_at, row.completed_at.as_deref()?))
        .collect();

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
            JobDay {
                partial: day == today,
                created: of_day.len() as i64,
                completed: count(&of_day, is_completed),
                day,
            }
        })
        .collect();

    Jobs {
        total: now_rows.len() as i64,
        completed: count(&now_rows, is_completed),
        failed: count(&now_rows, is_failed),
        interrupted: count(&now_rows, is_interrupted),
        live: count(&now_rows, |row| {
            crate::execution::job::LIVE_STATUSES.contains(&row.status.as_str())
        }),
        completion_rate: completion_rate(&now_rows),
        prev_completion_rate: completion_rate(&prev_rows),
        duration_s: stats::dist(durations),
        daily,
        stuck,
    }
}

pub(crate) async fn read(
    pool: &SqlitePool,
    project: Option<&str>,
    window: Window,
    now: DateTime<Utc>,
) -> sqlx::Result<Jobs> {
    let rows: Vec<Row> = sqlx::query_as(JOB_ROWS_SQL)
        .bind(window.previous().from_bound())
        .bind(window.to_bound())
        .bind(project)
        .fetch_all(pool)
        .await?;
    let live: Vec<LiveJob> = sqlx::query_as(LIVE_JOBS_SQL)
        .bind(project)
        .fetch_all(pool)
        .await?;
    let items: Vec<(i64, String, i64)> = sqlx::query_as(LIVE_ITEMS_SQL)
        .bind(project)
        .fetch_all(pool)
        .await?;
    Ok(jobs_block(&rows, stuck_block(&live, &items, now), window))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analytics::tests::now;
    use crate::execution::job::LIVE_STATUSES;
    use chrono::Duration;

    #[derive(Clone)]
    struct Job {
        project: &'static str,
        status: &'static str,
        rule_name: Option<&'static str>,
        budget_usd: Option<f64>,
        wait_reason: Option<&'static str>,
        round: i64,
        max_rounds: Option<i64>,
        dry_rounds: i64,
        gate_retries: i64,
        created: String,
        completed: Option<String>,
    }

    /// A scheduled job with nothing wrong with it.
    fn job(project: &'static str, status: &'static str, created: &str) -> Job {
        Job {
            project,
            status,
            rule_name: Some("nightly"),
            budget_usd: None,
            wait_reason: None,
            round: 0,
            max_rounds: None,
            dry_rounds: 0,
            gate_retries: 0,
            created: created.to_owned(),
            completed: None,
        }
    }

    /// A job created `age` before the test clock.
    fn aged(project: &'static str, status: &'static str, age: Duration) -> Job {
        job(project, status, &(now() - age).to_rfc3339())
    }

    async fn insert(pool: &SqlitePool, j: Job) -> i64 {
        sqlx::query_scalar(
            "INSERT INTO jobs (project_id, project_root, rule_name, status, max_items, wait_reason,
                               round, max_rounds, dry_rounds, gate_retries, budget_usd,
                               created_at, completed_at)
             VALUES (?, '/r', ?, ?, 3, ?, ?, ?, ?, ?, ?, ?, ?) RETURNING id",
        )
        .bind(j.project)
        .bind(j.rule_name)
        .bind(j.status)
        .bind(j.wait_reason)
        .bind(j.round)
        .bind(j.max_rounds)
        .bind(j.dry_rounds)
        .bind(j.gate_retries)
        .bind(j.budget_usd)
        .bind(j.created)
        .bind(j.completed)
        .fetch_one(pool)
        .await
        .unwrap()
    }

    async fn item(pool: &SqlitePool, job_id: i64, ordinal: i64, status: &str, attempts: i64) {
        sqlx::query(
            "INSERT INTO job_items (job_id, ordinal, description, status, gate_attempts)
             VALUES (?, ?, 'work', ?, ?)",
        )
        .bind(job_id)
        .bind(ordinal)
        .bind(status)
        .bind(attempts)
        .execute(pool)
        .await
        .unwrap();
    }

    async fn block_of(pool: &SqlitePool) -> Jobs {
        let window = Window::new(7, now()).unwrap();
        read(pool, None, window, now()).await.unwrap()
    }

    /// Spec §11: the stuck signals look only at live jobs and read the code's constants.
    #[tokio::test]
    async fn jobs_stuck_signals_apply_only_to_live_jobs_and_use_the_code_constants() {
        let pool = crate::testdb::fresh_pool().await;
        let past_four_hours = MAX_JOB_LIFETIME + Duration::hours(1);
        let hour = Duration::hours(1);
        // The commissioned lifetime is longer than the scheduled one, so the same age is fine.
        assert!(past_four_hours < COMMISSIONED_JOB_LIFETIME);

        let old = insert(&pool, aged("a", "implementing", past_four_hours)).await;
        let commissioned = Job {
            rule_name: None,
            budget_usd: Some(5.0),
            ..aged("b", "implementing", past_four_hours)
        };
        insert(&pool, commissioned).await;
        let dry = Job {
            dry_rounds: DRY_ROUNDS_TO_STOP,
            ..aged("c", "implementing", hour)
        };
        let dry = insert(&pool, dry).await;
        let exhausted = Job {
            round: 3,
            max_rounds: Some(3),
            ..aged("d", "implementing", hour)
        };
        let exhausted = insert(&pool, exhausted).await;
        let retrying = Job {
            gate_retries: 1,
            ..aged("e", "gating", hour)
        };
        let retrying = insert(&pool, retrying).await;
        item(&pool, retrying, 1, "gate_failed", 1).await;
        let conflicted = insert(&pool, aged("f", "implementing", hour)).await;
        item(&pool, conflicted, 1, "conflicted", 0).await;

        // Everything above at once, but finished: not live, so nothing counts.
        let done = Job {
            dry_rounds: DRY_ROUNDS_TO_STOP,
            round: 3,
            max_rounds: Some(3),
            gate_retries: 1,
            ..aged("g", "completed", past_four_hours)
        };
        let done = insert(&pool, done).await;
        item(&pool, done, 1, "gate_failed", 1).await;
        item(&pool, done, 2, "conflicted", 0).await;

        let stuck = block_of(&pool).await.stuck;
        assert_eq!(stuck.age_over_lifetime, 1);
        assert_eq!(stuck.dry_rounds_at_limit, 1);
        assert_eq!(stuck.rounds_exhausted, 1);
        assert_eq!(stuck.gate_retry_loop, 1);
        assert_eq!(stuck.item_stuck, 1);
        assert_eq!(stuck.waiting, Waiting::default());
        let listed: Vec<(i64, &str)> = stuck
            .jobs
            .iter()
            .map(|j| (j.id, j.project_id.as_str()))
            .collect();
        let expected = [
            (old, "a"),
            (dry, "c"),
            (exhausted, "d"),
            (retrying, "e"),
            (conflicted, "f"),
        ];
        assert_eq!(listed, expected);
    }

    /// Spec §11: `waiting` is grouped by every reason, known keys present at 0.
    #[tokio::test]
    async fn jobs_waiting_groups_by_every_wait_reason() {
        let pool = crate::testdb::fresh_pool().await;
        let reasons = [
            ("a", Some("slot")),
            ("b", Some("quota")),
            ("c", Some("kill-switch")),
            ("d", None),
        ];
        for (project, reason) in reasons {
            let waiting = Job {
                wait_reason: reason,
                ..aged(project, "waiting", Duration::hours(1))
            };
            insert(&pool, waiting).await;
        }
        let waiting = block_of(&pool).await.stuck.waiting;
        let expected = Waiting {
            slot: 1,
            quota: 1,
            kill_switch: 1,
            other: 1,
            ..Waiting::default()
        };
        assert_eq!(waiting, expected);
        let json = serde_json::to_value(&waiting).unwrap();
        assert_eq!(json["kill-switch"], 1);
        assert_eq!(json["budget"], 0);
    }

    /// Outcomes over the window by `created_at`, the rate and the duration of finished jobs.
    #[tokio::test]
    async fn jobs_outcomes_completion_rate_and_duration_by_created_at() {
        let pool = crate::testdb::fresh_pool().await;
        let finished = |status, created: &str, completed: Option<&str>| Job {
            completed: completed.map(str::to_owned),
            ..job("p", status, created)
        };
        let rows = [
            // In the window. Only the two completed jobs carry a `completed_at` here, so the
            // duration is taken over exactly those (1 h and 3 h).
            finished(
                "completed",
                "2026-10-15T10:00:00+00:00",
                Some("2026-10-15T11:00:00+00:00"),
            ),
            finished(
                "completed",
                "2026-10-16T09:00:00+00:00",
                Some("2026-10-16T12:00:00+00:00"),
            ),
            finished("failed", "2026-10-14T10:00:00+00:00", None),
            finished("gate_errored", "2026-10-13T10:00:00+00:00", None),
            finished("expired", "2026-10-12T10:00:00+00:00", None),
            job("q", "implementing", "2026-10-17T09:00:00+00:00"),
            // The window before.
            finished("completed", "2026-10-08T10:00:00+00:00", None),
            finished("failed", "2026-10-09T10:00:00+00:00", None),
        ];
        for row in rows {
            insert(&pool, row).await;
        }
        let block = block_of(&pool).await;
        assert_eq!(block.total, 6);
        assert_eq!(block.completed, 2);
        assert_eq!(block.failed, 2);
        assert_eq!(block.interrupted, 1);
        assert_eq!(block.live, 1);
        assert_eq!(block.completion_rate, Some(0.4));
        assert_eq!(block.prev_completion_rate, Some(0.5));
        assert_eq!((block.duration_s.p50, block.duration_s.n), (Some(3600), 2));
        assert_eq!(block.daily.len(), 7);
        let last = block.daily.last().unwrap();
        assert_eq!((last.created, last.completed, last.partial), (1, 0, true));
    }

    /// The SQL lists are text (sqlx refuses SQL built at run time), so a status added to the code
    /// has to fail here instead of quietly dropping out of the stuck signals.
    #[test]
    fn the_live_list_here_is_job_live_statuses() {
        let list = format!("('{}')", LIVE_STATUSES.join("','"));
        assert_eq!(LIVE_SQL_LIST, list);
        assert!(LIVE_JOBS_SQL.contains(&list));
        assert!(LIVE_ITEMS_SQL.contains(&list));
    }
}
