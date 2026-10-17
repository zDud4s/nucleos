//! The delivered block: merges the queue completed, per day, and the median time from a run's
//! start to its merge (spec §4.7).

use chrono::DateTime;
use serde::Serialize;
use sqlx::SqlitePool;

use super::Window;
use super::sessions::day_of;

/// Merges the queue completed. The median is over the window only, never per day.
#[derive(Debug, Clone, PartialEq, Default, Serialize)]
pub struct DeliveredTotal {
    pub landed: i64,
    /// Of those, the merges with a run behind them and a non-negative span.
    pub timed: i64,
    /// Median minutes run start → merge; `None` when nothing could be timed.
    pub median_minutes: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DeliveredPoint {
    pub day: String,
    pub partial: bool,
    pub landed: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DeliveredBlock {
    pub total: DeliveredTotal,
    pub previous: DeliveredTotal,
    pub series: Vec<DeliveredPoint>,
}

/// One succeeded merge on the UTC day it finished, with its span when it can be timed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergeLine {
    pub day: String,
    pub minutes: Option<i64>,
}

/// `project/readings.rs` `delivered`'s rule over two windows at once: succeeded merges by
/// `finished_at`, timed from the run's `created_at`, a negative span dropped rather than clamped.
pub async fn merge_lines(
    pool: &SqlitePool,
    project: Option<&str>,
    from: &str,
    to: &str,
) -> sqlx::Result<Vec<MergeLine>> {
    let raw: Vec<(String, Option<String>)> = sqlx::query_as(
        "SELECT v.finished_at, r.created_at
           FROM vcs_requests v LEFT JOIN runs r ON r.id = v.run_id
          WHERE v.op = 'merge' AND v.status = 'succeeded'
            AND v.finished_at IS NOT NULL AND v.finished_at >= ?1 AND v.finished_at < ?2
            AND (?3 IS NULL OR v.project_id = ?3)",
    )
    .bind(from)
    .bind(to)
    .bind(project)
    .fetch_all(pool)
    .await?;
    Ok(raw
        .into_iter()
        .map(|(finished, started)| {
            let minutes = started.as_deref().and_then(|started| {
                let started = DateTime::parse_from_rfc3339(started).ok()?;
                let finished = DateTime::parse_from_rfc3339(&finished).ok()?;
                let minutes = (finished - started).num_minutes();
                (minutes >= 0).then_some(minutes)
            });
            MergeLine {
                day: day_of(&finished).to_owned(),
                minutes,
            }
        })
        .collect())
}

fn delivered_total(window: Window, merges: &[MergeLine]) -> DeliveredTotal {
    let landed: Vec<&MergeLine> = merges.iter().filter(|m| window.holds(&m.day)).collect();
    let minutes: Vec<i64> = landed.iter().filter_map(|m| m.minutes).collect();
    DeliveredTotal {
        landed: landed.len() as i64,
        timed: minutes.len() as i64,
        median_minutes: crate::project::readings::median_of(&minutes).map(|m| m as f64),
    }
}

pub fn delivered_block(window: Window, merges: &[MergeLine]) -> DeliveredBlock {
    let days = window.day_list();
    let last = days.len() - 1;
    let series = days
        .iter()
        .enumerate()
        .map(|(index, day)| DeliveredPoint {
            day: day.clone(),
            partial: index == last,
            landed: merges.iter().filter(|m| m.day == *day).count() as i64,
        })
        .collect();
    DeliveredBlock {
        total: delivered_total(window, merges),
        previous: delivered_total(window.previous(), merges),
        series,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analytics::Scope;
    use crate::analytics::tests::{Run, done, insert, report_of};

    async fn insert_merge(
        pool: &SqlitePool,
        project: &str,
        status: &str,
        run_id: Option<i64>,
        finished_at: &str,
    ) {
        sqlx::query(
            "INSERT INTO vcs_requests (op, args, project_id, project_root, repo_key, origin,
                                       run_id, status, created_at, finished_at)
             VALUES ('merge', '{}', ?, 'C:/repo', 'k', 'run', ?, ?, ?, ?)",
        )
        .bind(project)
        .bind(run_id)
        .bind(status)
        .bind(finished_at)
        .bind(finished_at)
        .execute(pool)
        .await
        .unwrap();
    }

    /// Spec test 6: a merge with no run is landed but not timed, a negative span is dropped, and a
    /// median over nothing is nothing.
    #[tokio::test]
    async fn deliveries_count_every_merge_and_time_only_the_honest_ones() {
        let pool = crate::testdb::fresh_pool().await;
        let timed = insert(
            &pool,
            Run {
                created: "2026-10-05T10:00:00Z",
                ..done("alpha", "2026-10-05T10:30:00Z")
            },
        )
        .await;
        let backwards = insert(
            &pool,
            Run {
                created: "2026-10-06T12:00:00Z",
                ..done("alpha", "2026-10-06T12:30:00Z")
            },
        )
        .await;
        insert_merge(
            &pool,
            "alpha",
            "succeeded",
            Some(timed),
            "2026-10-05T11:30:00Z",
        )
        .await;
        insert_merge(
            &pool,
            "alpha",
            "succeeded",
            Some(backwards),
            "2026-10-06T11:00:00Z",
        )
        .await;
        insert_merge(&pool, "alpha", "succeeded", None, "2026-10-07T09:00:00Z").await;
        insert_merge(&pool, "alpha", "failed", None, "2026-10-07T10:00:00Z").await;
        insert_merge(&pool, "beta", "succeeded", None, "2026-10-07T11:00:00Z").await;

        let report = report_of(&pool, Scope::Project("alpha"), 30).await;

        assert_eq!(
            report.delivered.total,
            DeliveredTotal {
                landed: 3,
                timed: 1,
                median_minutes: Some(90.0)
            }
        );
        assert_eq!(
            report.delivered.previous,
            DeliveredTotal {
                landed: 0,
                timed: 0,
                median_minutes: None
            }
        );
        assert_eq!(report.delivered.series.len(), 30);
        let landed_on = |day: &str| {
            report
                .delivered
                .series
                .iter()
                .find(|p| p.day == day)
                .unwrap()
                .landed
        };
        assert_eq!((landed_on("2026-10-05"), landed_on("2026-10-07")), (1, 1));

        let global = report_of(&pool, Scope::Global, 30).await;
        assert_eq!(global.delivered.total.landed, 4);
    }
}
