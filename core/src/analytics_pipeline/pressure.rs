//! Metric 10, slot pressure (spec analytics-pipeline §6.1 "10. Pressão"): how much of the window
//! the scope sat at its slot limit, from raw `slot_samples` for 7 days and `slot_samples_daily`
//! for 30 and 90 (raw rows are kept 8 days), plus the share of the window's jobs that waited for
//! a slot.
//!
//! A project skips its idle ticks (spec §5.3), so its own rows cannot say how long it was idle.
//! Every share and average here is read against the house's samples, which are written every tick,
//! and a house tick with no row for the project counts as 0 in use.

use std::collections::BTreeMap;

use serde::Serialize;
use sqlx::SqlitePool;

use super::stats::{self, ratio};
use crate::analytics::Window;

/// Windows up to this many days read the raw samples; longer ones read the daily rollup.
const RAW_DAYS: i64 = 7;

/// The raw ticks of a window: the house's rows always, the project's when one is asked for. `?3`
/// NULL is the house alone.
pub(crate) const RAW_SAMPLES_SQL: &str = "SELECT sampled_at, project_id, in_use, slot_limit
   FROM slot_samples
  WHERE sampled_at >= ?1 AND sampled_at < ?2 AND (project_id IS NULL OR project_id = ?3)
  ORDER BY sampled_at";

/// The daily rows of both windows, `[previous.from, window.to)`: the house (`''`) always, the
/// project's when one is asked for.
pub(crate) const DAILY_SAMPLES_SQL: &str = "SELECT day, project_id, samples, avg_in_use,
        max_in_use, slot_limit, at_limit_samples
   FROM slot_samples_daily
  WHERE day >= ?1 AND day < ?2 AND (project_id = '' OR project_id = ?3)
  ORDER BY day";

/// The scope's latest limit: the house's (`?1` NULL) or the project's.
pub(crate) const LATEST_LIMIT_SQL: &str = "SELECT slot_limit FROM slot_samples
  WHERE project_id IS ?1 ORDER BY sampled_at DESC LIMIT 1";

/// The window's jobs, and how many of them wait for a slot right now.
pub(crate) const SLOT_WAIT_SQL: &str =
    "SELECT COUNT(*), COALESCE(SUM(wait_reason = 'slot'), 0) FROM jobs
  WHERE created_at >= ?1 AND created_at < ?2 AND (?3 IS NULL OR project_id = ?3)";

#[derive(Debug, Clone, sqlx::FromRow)]
pub(crate) struct RawRow {
    pub sampled_at: String,
    pub project_id: Option<String>,
    pub in_use: i64,
    pub slot_limit: i64,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub(crate) struct DailyRow {
    pub day: String,
    pub project_id: String,
    pub samples: i64,
    pub avg_in_use: f64,
    pub max_in_use: i64,
    pub slot_limit: i64,
    pub at_limit_samples: i64,
}

/// One point of the 7-day series: the p95 of the scope's in-use over the day's house ticks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct P95Day {
    pub day: String,
    pub partial: bool,
    pub in_use: Option<i64>,
}

/// One point of the daily series, every window: `null` on a day with no house sample.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PressureDay {
    pub day: String,
    pub partial: bool,
    pub avg_in_use: Option<f64>,
    pub max_in_use: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Pressure {
    /// House ticks the scope spent at its limit (`in_use >= limit`, `limit > 0`), over the
    /// window's house ticks. `null` with no sample, never 0.
    pub at_limit_share: Option<f64>,
    /// The previous window, always from the daily rows: the 7-day one is past raw retention.
    pub prev_at_limit_share: Option<f64>,
    pub peak_in_use: Option<i64>,
    pub prev_peak_in_use: Option<i64>,
    /// The house's limit globally, the project's latest one in a project.
    pub limit: Option<i64>,
    /// 7 days only (raw ticks); `null` for 30 and 90, whose daily rows keep no percentile.
    pub daily_p95: Option<Vec<P95Day>>,
    /// Exactly `days` points; `partial` on today.
    pub daily: Vec<PressureDay>,
    /// Window jobs waiting for a slot over window jobs.
    pub slot_wait_share: Option<f64>,
    /// Always true: `wait_reason` is cleared when a job stops waiting, so only jobs still waiting
    /// for a slot are seen.
    pub slot_wait_share_is_lower_bound: bool,
}

/// One UTC day of a scope, counted on the house's clock.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct DayFigures {
    /// House ticks that day, the denominator of every share and average.
    ticks: i64,
    /// Ticks the scope spent at its limit.
    at_limit: i64,
    /// The scope's in-use summed over the ticks; a tick it skipped adds 0.
    in_use_sum: f64,
    max_in_use: i64,
}

/// One house tick and what the scope held at it.
#[derive(Debug, Clone, Copy, Default)]
struct Tick {
    house: bool,
    in_use: i64,
    at_limit: bool,
}

fn at_limit(in_use: i64, limit: i64) -> bool {
    limit > 0 && in_use >= limit
}

/// PURE: per day, the figures and the in-use value of every house tick (for the p95).
fn raw_days(rows: &[RawRow], project: Option<&str>) -> BTreeMap<String, (DayFigures, Vec<i64>)> {
    let mut ticks: BTreeMap<&str, Tick> = BTreeMap::new();
    for row in rows {
        let tick = ticks.entry(row.sampled_at.as_str()).or_default();
        if row.project_id.is_none() {
            tick.house = true;
        }
        if row.project_id.as_deref() == project {
            tick.in_use = row.in_use;
            tick.at_limit = at_limit(row.in_use, row.slot_limit);
        }
    }
    let mut days: BTreeMap<String, (DayFigures, Vec<i64>)> = BTreeMap::new();
    for (at, tick) in ticks {
        let Some(day) = at.get(..10) else {
            continue;
        };
        // A project row with no house row beside it has no clock to be read against.
        if !tick.house {
            continue;
        }
        let (figures, values) = days.entry(day.to_owned()).or_default();
        figures.ticks += 1;
        figures.at_limit += i64::from(tick.at_limit);
        figures.in_use_sum += tick.in_use as f64;
        figures.max_in_use = figures.max_in_use.max(tick.in_use);
        values.push(tick.in_use);
    }
    days
}

/// PURE: per day, the scope's rollup read against the house's `samples`. A day with a house row
/// and no project row is a day the project spent idle.
fn daily_days(rows: &[DailyRow], project: Option<&str>) -> BTreeMap<String, DayFigures> {
    let scope = project.unwrap_or("");
    let mut houses: BTreeMap<&str, i64> = BTreeMap::new();
    let mut scoped: BTreeMap<&str, &DailyRow> = BTreeMap::new();
    for row in rows {
        if row.project_id.is_empty() {
            houses.insert(row.day.as_str(), row.samples);
        }
        if row.project_id == scope {
            scoped.insert(row.day.as_str(), row);
        }
    }
    houses
        .into_iter()
        .map(|(day, ticks)| {
            let figures = match scoped.get(day) {
                Some(row) => DayFigures {
                    ticks,
                    at_limit: row.at_limit_samples,
                    in_use_sum: row.avg_in_use * row.samples as f64,
                    max_in_use: row.max_in_use,
                },
                None => DayFigures {
                    ticks,
                    ..DayFigures::default()
                },
            };
            (day.to_owned(), figures)
        })
        .collect()
}

/// The share at the limit and the peak over the days `window` holds.
fn summary(days: &BTreeMap<String, DayFigures>, window: Window) -> (Option<f64>, Option<i64>) {
    let held: Vec<&DayFigures> = days
        .iter()
        .filter(|(day, figures)| window.holds(day) && figures.ticks > 0)
        .map(|(_, figures)| figures)
        .collect();
    let at_limit: i64 = held.iter().map(|figures| figures.at_limit).sum();
    let ticks: i64 = held.iter().map(|figures| figures.ticks).sum();
    let peak = held.iter().map(|figures| figures.max_in_use).max();
    (ratio(at_limit, ticks), peak)
}

fn series(days: &BTreeMap<String, DayFigures>, window: Window) -> Vec<PressureDay> {
    let today = window.span().to;
    window
        .day_list()
        .into_iter()
        .map(|day| {
            let figures = days.get(&day).filter(|figures| figures.ticks > 0);
            PressureDay {
                partial: day == today,
                avg_in_use: figures.map(|f| f.in_use_sum / f.ticks as f64),
                max_in_use: figures.map(|f| f.max_in_use),
                day,
            }
        })
        .collect()
}

/// The scope's limit when no raw sample is left: its latest daily row.
fn daily_limit(rows: &[DailyRow], project: Option<&str>) -> Option<i64> {
    let scope = project.unwrap_or("");
    rows.iter()
        .filter(|row| row.project_id == scope)
        .max_by(|a, b| a.day.cmp(&b.day))
        .map(|row| row.slot_limit)
}

/// What the block needs besides the samples.
pub(crate) struct Extras {
    /// The scope's latest limit from the raw samples, when there is one.
    pub latest_limit: Option<i64>,
    /// `(window jobs, of them waiting for a slot)`.
    pub jobs: (i64, i64),
}

/// PURE: the block. `raw` holds the window's ticks for 7 days and nothing otherwise; `daily` holds
/// both windows' rows.
pub(crate) fn pressure_block(
    raw: &[RawRow],
    daily: &[DailyRow],
    project: Option<&str>,
    window: Window,
    extras: Extras,
) -> Pressure {
    let rolled = daily_days(daily, project);
    let (prev_at_limit_share, prev_peak_in_use) = summary(&rolled, window.previous());
    let (at_limit_share, peak_in_use, daily_p95, points) = if window.days <= RAW_DAYS {
        let by_day = raw_days(raw, project);
        let figures: BTreeMap<String, DayFigures> = by_day
            .iter()
            .map(|(day, (figures, _))| (day.clone(), *figures))
            .collect();
        let (share, peak) = summary(&figures, window);
        let today = window.span().to;
        let p95: Vec<P95Day> = window
            .day_list()
            .into_iter()
            .map(|day| P95Day {
                partial: day == today,
                in_use: by_day
                    .get(&day)
                    .and_then(|(_, values)| stats::dist(values.clone()).p95),
                day,
            })
            .collect();
        (share, peak, Some(p95), series(&figures, window))
    } else {
        let (share, peak) = summary(&rolled, window);
        (share, peak, None, series(&rolled, window))
    };
    let (jobs, waiting_for_a_slot) = extras.jobs;
    Pressure {
        at_limit_share,
        prev_at_limit_share,
        peak_in_use,
        prev_peak_in_use,
        limit: extras.latest_limit.or_else(|| daily_limit(daily, project)),
        daily_p95,
        daily: points,
        slot_wait_share: ratio(waiting_for_a_slot, jobs),
        slot_wait_share_is_lower_bound: true,
    }
}

pub(crate) async fn read(
    pool: &SqlitePool,
    project: Option<&str>,
    window: Window,
) -> sqlx::Result<Pressure> {
    let raw: Vec<RawRow> = if window.days <= RAW_DAYS {
        sqlx::query_as(RAW_SAMPLES_SQL)
            .bind(window.from_bound())
            .bind(window.to_bound())
            .bind(project)
            .fetch_all(pool)
            .await?
    } else {
        Vec::new()
    };
    let daily: Vec<DailyRow> = sqlx::query_as(DAILY_SAMPLES_SQL)
        .bind(window.previous().from_bound())
        .bind(window.to_bound())
        .bind(project)
        .fetch_all(pool)
        .await?;
    let latest_limit: Option<i64> = sqlx::query_scalar(LATEST_LIMIT_SQL)
        .bind(project)
        .fetch_optional(pool)
        .await?;
    let jobs: (i64, i64) = sqlx::query_as(SLOT_WAIT_SQL)
        .bind(window.from_bound())
        .bind(window.to_bound())
        .bind(project)
        .fetch_one(pool)
        .await?;
    let extras = Extras { latest_limit, jobs };
    Ok(pressure_block(&raw, &daily, project, window, extras))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analytics::tests::{close, now};

    async fn raw(pool: &SqlitePool, rows: &[(&str, Option<&str>, i64, i64)]) {
        for (at, project, in_use, limit) in rows {
            sqlx::query(
                "INSERT INTO slot_samples (sampled_at, project_id, in_use, slot_limit)
                 VALUES (?, ?, ?, ?)",
            )
            .bind(at)
            .bind(project)
            .bind(in_use)
            .bind(limit)
            .execute(pool)
            .await
            .unwrap();
        }
    }

    /// `(day, project ('' = house), samples, avg_in_use, max_in_use, slot_limit, at_limit)`
    async fn daily(pool: &SqlitePool, rows: &[(&str, &str, i64, f64, i64, i64, i64)]) {
        for (day, project, samples, avg, max, limit, at_limit) in rows {
            sqlx::query(
                "INSERT INTO slot_samples_daily
                     (day, project_id, samples, avg_in_use, max_in_use, slot_limit,
                      at_limit_samples)
                 VALUES (?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(day)
            .bind(project)
            .bind(samples)
            .bind(avg)
            .bind(max)
            .bind(limit)
            .bind(at_limit)
            .execute(pool)
            .await
            .unwrap();
        }
    }

    async fn job(pool: &SqlitePool, project: &str, status: &str, wait: Option<&str>, at: &str) {
        sqlx::query(
            "INSERT INTO jobs (project_id, project_root, status, max_items, wait_reason, created_at)
             VALUES (?, '/r', ?, 3, ?, ?)",
        )
        .bind(project)
        .bind(status)
        .bind(wait)
        .bind(at)
        .execute(pool)
        .await
        .unwrap();
    }

    async fn pressure(pool: &SqlitePool, project: Option<&str>, days: i64) -> Pressure {
        read(pool, project, Window::new(days, now()).unwrap())
            .await
            .unwrap()
    }

    /// Spec §6.1, 10: 7 days read the raw ticks, the previous 7 the daily rows (past raw
    /// retention); 30 and 90 read only the daily rows and carry no p95.
    #[tokio::test]
    async fn pressure_reads_raw_samples_for_seven_days_and_daily_rows_for_thirty_and_ninety() {
        let pool = crate::testdb::fresh_pool().await;
        // Five house ticks on the 16th, two at the limit of 4.
        raw(
            &pool,
            &[
                ("2026-10-16T10:00:00Z", None, 4, 4),
                ("2026-10-16T10:01:00Z", None, 4, 4),
                ("2026-10-16T10:02:00Z", None, 2, 4),
                ("2026-10-16T10:03:00Z", None, 1, 4),
                ("2026-10-16T10:04:00Z", None, 0, 4),
            ],
        )
        .await;
        // The same day rolled with other numbers, so which source was read shows.
        daily(
            &pool,
            &[
                ("2026-08-01", "", 20, 0.5, 1, 4, 0),
                ("2026-10-08", "", 10, 2.0, 4, 4, 5),
                ("2026-10-16", "", 10, 1.5, 3, 4, 1),
            ],
        )
        .await;

        let week = pressure(&pool, None, 7).await;
        assert_eq!(week.at_limit_share, Some(0.4), "two of five raw ticks");
        assert_eq!(week.peak_in_use, Some(4));
        assert_eq!(week.limit, Some(4));
        assert_eq!(week.prev_at_limit_share, Some(0.5), "the daily 8th");
        assert_eq!(week.prev_peak_in_use, Some(4));
        let p95 = week.daily_p95.as_ref().expect("7 days carry the p95");
        assert_eq!(p95.len(), 7);
        let sixteenth = p95.iter().find(|point| point.day == "2026-10-16").unwrap();
        assert_eq!(sixteenth.in_use, Some(4));
        assert!(p95.last().unwrap().partial);
        let sixteenth = week.daily.iter().find(|p| p.day == "2026-10-16").unwrap();
        assert!(close(sixteenth.avg_in_use.unwrap(), 2.2));
        assert_eq!(sixteenth.max_in_use, Some(4));

        let month = pressure(&pool, None, 30).await;
        assert_eq!(month.at_limit_share, Some(0.3), "6 / 20, daily rows");
        assert_eq!(month.peak_in_use, Some(4));
        assert!(month.daily_p95.is_none(), "daily rows keep no percentile");
        assert_eq!(month.daily.len(), 30);
        let sixteenth = month.daily.iter().find(|p| p.day == "2026-10-16").unwrap();
        assert_eq!(
            (sixteenth.avg_in_use, sixteenth.max_in_use),
            (Some(1.5), Some(3))
        );

        let quarter = pressure(&pool, None, 90).await;
        assert_eq!(quarter.at_limit_share, Some(0.15), "6 / 40 with August");
        assert_eq!(quarter.daily.len(), 90);
    }

    /// Spec §5.3 and §6.1, 10: a project skips its idle ticks, so its share is over the house's
    /// ticks and a tick with no project row is 0 in use, raw and daily alike.
    #[tokio::test]
    async fn a_project_at_limit_share_is_read_against_the_house_samples() {
        let pool = crate::testdb::fresh_pool().await;
        let ticks = [
            "2026-10-16T10:00:00Z",
            "2026-10-16T10:01:00Z",
            "2026-10-16T10:02:00Z",
            "2026-10-16T10:03:00Z",
            "2026-10-16T10:04:00Z",
        ];
        for at in ticks {
            raw(&pool, &[(at, None, 3, 8), (at, Some("q"), 2, 2)]).await;
        }
        // `p` at its limit twice, then one in use, then idle and skipped.
        raw(
            &pool,
            &[
                (ticks[0], Some("p"), 2, 2),
                (ticks[1], Some("p"), 2, 2),
                (ticks[2], Some("p"), 1, 2),
            ],
        )
        .await;

        let week = pressure(&pool, Some("p"), 7).await;
        // Two of five house ticks, not two of the project's three rows.
        assert_eq!(week.at_limit_share, Some(0.4));
        assert_eq!(week.peak_in_use, Some(2));
        assert_eq!(week.limit, Some(2), "the project's, not the house's");
        let p95 = week.daily_p95.unwrap();
        let sixteenth = p95.iter().find(|point| point.day == "2026-10-16").unwrap();
        assert_eq!(sixteenth.in_use, Some(2), "over [0, 0, 1, 2, 2]");
        let sixteenth = week.daily.iter().find(|p| p.day == "2026-10-16").unwrap();
        assert!(close(sixteenth.avg_in_use.unwrap(), 1.0), "5 over 5 ticks");

        // The rollup the same way: `p`'s 360 samples a day against the house's 1440.
        daily(
            &pool,
            &[
                ("2026-10-01", "", 1440, 3.0, 4, 8, 0),
                ("2026-10-01", "p", 360, 2.0, 2, 2, 144),
                ("2026-10-02", "", 1440, 3.0, 4, 8, 0),
            ],
        )
        .await;
        let month = pressure(&pool, Some("p"), 30).await;
        assert_eq!(
            month.at_limit_share,
            Some(0.05),
            "144 over both days' 2880 house samples"
        );
        let first = month.daily.iter().find(|p| p.day == "2026-10-01").unwrap();
        assert!(close(first.avg_in_use.unwrap(), 0.5), "2.0 * 360 / 1440");
        let second = month.daily.iter().find(|p| p.day == "2026-10-02").unwrap();
        assert_eq!(
            (second.avg_in_use, second.max_in_use),
            (Some(0.0), Some(0)),
            "a house day without a project row is a day the project was idle"
        );
    }

    /// Spec §6.1, 10: `wait_reason` is current state (cleared when a job stops waiting), so the
    /// share counts the jobs still waiting for a slot and says it is a lower bound.
    #[tokio::test]
    async fn slot_wait_share_counts_live_waiting_jobs_and_says_it_is_a_lower_bound() {
        let pool = crate::testdb::fresh_pool().await;
        let at = "2026-10-16T10:00:00+00:00";
        job(&pool, "p", "waiting", Some("slot"), at).await;
        job(&pool, "p", "waiting", Some("budget"), at).await;
        job(&pool, "p", "completed", None, at).await;
        job(&pool, "q", "waiting", Some("slot"), at).await;
        let before = "2026-10-05T10:00:00+00:00";
        job(&pool, "p", "waiting", Some("slot"), before).await;

        let global = pressure(&pool, None, 7).await;
        assert_eq!(global.slot_wait_share, Some(0.5), "two of four jobs");
        assert!(global.slot_wait_share_is_lower_bound);
        let project = pressure(&pool, Some("p"), 7).await;
        assert_eq!(project.slot_wait_share, Some(1.0 / 3.0));
    }

    /// Spec §6.1, 10 and §8: a window without samples has no share, no peak and empty points,
    /// never zeros.
    #[tokio::test]
    async fn no_samples_in_the_window_is_no_history_not_zero() {
        let pool = crate::testdb::fresh_pool().await;
        for days in [7, 30, 90] {
            let block = pressure(&pool, None, days).await;
            assert_eq!(block.at_limit_share, None, "{days}");
            assert_eq!(block.prev_at_limit_share, None, "{days}");
            assert_eq!(block.peak_in_use, None, "{days}");
            assert_eq!(block.limit, None, "{days}");
            assert_eq!(block.slot_wait_share, None, "{days}");
            assert_eq!(block.daily.len() as i64, days);
            assert!(
                block
                    .daily
                    .iter()
                    .all(|p| p.avg_in_use.is_none() && p.max_in_use.is_none()),
                "{days}"
            );
            if let Some(p95) = &block.daily_p95 {
                assert!(p95.iter().all(|point| point.in_use.is_none()));
            }
        }
    }
}
