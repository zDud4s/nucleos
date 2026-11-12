//! The slot sampler (spec analytics-pipeline §5.3, §10): every 60 s, one row per project holding
//! or configured for slots plus one for the house, from one `concurrency::readout`; once a day,
//! the closed UTC days rolled up into `slot_samples_daily` and old rows pruned in batches.
//!
//! Nothing here sits on the `acquire`/`release` path. A failed tick logs and is skipped; it never
//! propagates and never stops the daemon.

use std::collections::HashMap;

use chrono::{DateTime, Duration, NaiveDate, SecondsFormat, Utc};
use sqlx::SqlitePool;

/// What one tick records: `(project_id, in_use, limit)` per project, and the house.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub house_in_use: i64,
    pub house_limit: i64,
    pub projects: Vec<(String, i64, i64)>,
}

/// The `(in_use, limit)` each project's last written row had, for the idle rule.
pub type Last = HashMap<String, (i64, i64)>;

/// The sampler's cadence: slots last minutes to hours, and the question is "time at the limit",
/// not "instantaneous peak" (spec §5.3).
pub const SAMPLE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60);

/// The tick as the readout describes it. `held` and `slots.len()` are the same counting the
/// fleet screen does, so the two never disagree.
pub fn snapshot_of(readout: &crate::execution::concurrency::Readout) -> Snapshot {
    Snapshot {
        house_in_use: readout.house.held,
        house_limit: readout.house.limit,
        projects: readout
            .projects
            .iter()
            .map(|project| {
                (
                    project.project_id.clone(),
                    project.slots.len() as i64,
                    project.limit,
                )
            })
            .collect(),
    }
}

/// Writes one tick and returns how many rows it wrote. Never fails: a failed readout or write is
/// logged and the tick is skipped (spec §5.3, "Falha").
pub async fn write_tick(
    pool: &SqlitePool,
    now: DateTime<Utc>,
    reading: sqlx::Result<Snapshot>,
    last: &mut Last,
) -> usize {
    let snapshot = match reading {
        Ok(snapshot) => snapshot,
        Err(error) => {
            tracing::warn!(%error, "slot sampler: readout failed; tick skipped");
            return 0;
        }
    };
    match insert_tick(pool, now, &snapshot, last).await {
        Ok(written) => written,
        Err(error) => {
            tracing::warn!(%error, "slot sampler: write failed; tick skipped");
            0
        }
    }
}

/// One transaction per tick, so a tick is all its rows or none. `last` is replaced only after
/// the commit: a rolled-back tick must not count as written for the idle rule.
async fn insert_tick(
    pool: &SqlitePool,
    now: DateTime<Utc>,
    snapshot: &Snapshot,
    last: &mut Last,
) -> sqlx::Result<usize> {
    let at = now.to_rfc3339_opts(SecondsFormat::Secs, true);
    let mut tx = pool.begin().await?;
    sqlx::query(
        "INSERT INTO slot_samples (sampled_at, project_id, in_use, slot_limit)
         VALUES (?, NULL, ?, ?)",
    )
    .bind(&at)
    .bind(snapshot.house_in_use)
    .bind(snapshot.house_limit)
    .execute(&mut *tx)
    .await?;
    let mut written = 1;
    let mut next = Last::with_capacity(snapshot.projects.len());
    for (project_id, in_use, limit) in &snapshot.projects {
        let pair = (*in_use, *limit);
        // Idle and unchanged since the last row: absence reads as 0 in use between house rows.
        let still_idle = *in_use == 0 && last.get(project_id) == Some(&pair);
        next.insert(project_id.clone(), pair);
        if still_idle {
            continue;
        }
        sqlx::query(
            "INSERT INTO slot_samples (sampled_at, project_id, in_use, slot_limit)
             VALUES (?, ?, ?, ?)",
        )
        .bind(&at)
        .bind(project_id)
        .bind(in_use)
        .bind(limit)
        .execute(&mut *tx)
        .await?;
        written += 1;
    }
    tx.commit().await?;
    *last = next;
    Ok(written)
}

/// Rolls one closed UTC day of raw samples into `slot_samples_daily`, one row per scope.
/// `INSERT OR REPLACE`, so running it twice is the same as once. Returns the rows written.
pub async fn rollup_day(pool: &SqlitePool, day: NaiveDate) -> sqlx::Result<u64> {
    let (from, to) = (day.to_string(), (day + Duration::days(1)).to_string());
    let done = sqlx::query(
        "INSERT OR REPLACE INTO slot_samples_daily
             (day, project_id, samples, avg_in_use, max_in_use, slot_limit, at_limit_samples)
         SELECT ?1, COALESCE(project_id, ''), COUNT(*), AVG(in_use), MAX(in_use), MAX(slot_limit),
                SUM(CASE WHEN slot_limit > 0 AND in_use >= slot_limit THEN 1 ELSE 0 END)
           FROM slot_samples
          WHERE sampled_at >= ?1 AND sampled_at < ?2
          GROUP BY COALESCE(project_id, '')",
    )
    .bind(&from)
    .bind(&to)
    .execute(pool)
    .await?;
    Ok(done.rows_affected())
}

/// Rolls up every closed day after the last daily row (or from the oldest raw sample) up to
/// yesterday, as far as raw rows still exist (spec §10, "Rollup"). Returns the days that had
/// samples. A day already pruned and never rolled stays without data, without an error.
pub async fn catch_up(pool: &SqlitePool, today: NaiveDate) -> sqlx::Result<u32> {
    let last_daily: Option<String> = sqlx::query_scalar("SELECT MAX(day) FROM slot_samples_daily")
        .fetch_one(pool)
        .await?;
    let first_raw: Option<String> = sqlx::query_scalar("SELECT MIN(sampled_at) FROM slot_samples")
        .fetch_one(pool)
        .await?;
    let Some(first_raw) = first_raw.as_deref().and_then(super::stats::day_of) else {
        return Ok(0);
    };
    let after_daily = last_daily
        .as_deref()
        .and_then(super::stats::day_of)
        .map(|last| last + Duration::days(1));
    let mut day = after_daily.map_or(first_raw, |after| after.max(first_raw));
    let mut rolled = 0;
    while day < today {
        if rollup_day(pool, day).await? > 0 {
            rolled += 1;
        }
        day += Duration::days(1);
    }
    Ok(rolled)
}

/// Raw samples: the 7-day window plus a day for the partial one (spec §10).
pub const RAW_RETENTION_DAYS: i64 = 8;
/// Daily rows: 90 days and the 90 before them, with room to spare.
pub const DAILY_RETENTION_DAYS: i64 = 400;
/// Transitions: a couple of rows per incident; a year is plenty.
pub const TRANSITION_RETENTION_DAYS: i64 = 365;
/// Rows per DELETE, so a sweep never holds SQLite's writer for long.
pub const PRUNE_BATCH: i64 = 5_000;

/// Runs `sql` (a `DELETE ... WHERE rowid IN (SELECT ... LIMIT ?2)` taking the cutoff as `?1`)
/// until a batch comes back short.
async fn delete_in_batches(
    pool: &SqlitePool,
    sql: &'static str,
    cutoff: &str,
    batch: i64,
) -> sqlx::Result<u64> {
    let mut total = 0;
    loop {
        let gone = sqlx::query(sql)
            .bind(cutoff)
            .bind(batch)
            .execute(pool)
            .await?
            .rows_affected();
        total += gone;
        if gone < batch as u64 {
            return Ok(total);
        }
    }
}

/// Raw rows of days before `today - 8`, and only of days that already have their daily row.
pub async fn prune_raw(pool: &SqlitePool, now: DateTime<Utc>, batch: i64) -> sqlx::Result<u64> {
    let cutoff = (now.date_naive() - Duration::days(RAW_RETENTION_DAYS)).to_string();
    delete_in_batches(
        pool,
        "DELETE FROM slot_samples WHERE rowid IN (
             SELECT s.rowid FROM slot_samples s
              WHERE s.sampled_at < ?1
                AND EXISTS (
                    SELECT 1 FROM slot_samples_daily d WHERE d.day = substr(s.sampled_at, 1, 10))
              LIMIT ?2)",
        &cutoff,
        batch,
    )
    .await
}

pub async fn prune_daily(pool: &SqlitePool, now: DateTime<Utc>, batch: i64) -> sqlx::Result<u64> {
    let cutoff = (now.date_naive() - Duration::days(DAILY_RETENTION_DAYS)).to_string();
    delete_in_batches(
        pool,
        "DELETE FROM slot_samples_daily WHERE rowid IN (
             SELECT rowid FROM slot_samples_daily WHERE day < ?1 LIMIT ?2)",
        &cutoff,
        batch,
    )
    .await
}

pub async fn prune_transitions(
    pool: &SqlitePool,
    now: DateTime<Utc>,
    batch: i64,
) -> sqlx::Result<u64> {
    let cutoff = (now.date_naive() - Duration::days(TRANSITION_RETENTION_DAYS)).to_string();
    delete_in_batches(
        pool,
        "DELETE FROM postgate_transitions WHERE id IN (
             SELECT id FROM postgate_transitions WHERE at < ?1 LIMIT ?2)",
        &cutoff,
        batch,
    )
    .await
}

/// The day's maintenance: roll up what closed, then prune. Each step logs its own failure and
/// the next still runs.
async fn daily_maintenance(pool: &SqlitePool, now: DateTime<Utc>) {
    if let Err(error) = catch_up(pool, now.date_naive()).await {
        tracing::warn!(%error, "slot sampler: daily rollup failed");
    }
    for (what, result) in [
        ("raw slot samples", prune_raw(pool, now, PRUNE_BATCH).await),
        (
            "daily slot samples",
            prune_daily(pool, now, PRUNE_BATCH).await,
        ),
        (
            "post-merge gate transitions",
            prune_transitions(pool, now, PRUNE_BATCH).await,
        ),
    ] {
        match result {
            Ok(0) => {}
            Ok(pruned) => tracing::info!(pruned, "analytics: {what} past the retention window"),
            Err(error) => tracing::warn!(%error, "analytics: pruning {what} failed"),
        }
    }
}

/// The sampler's loop, spawned once from `main.rs`. Catches up and prunes at start (a daemon
/// that only ever runs for an hour must still roll its days), then samples every minute and
/// repeats the maintenance whenever the UTC day changes.
pub async fn run_slot_sampler(pool: SqlitePool) {
    let mut ticker = tokio::time::interval(SAMPLE_INTERVAL);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut last = Last::new();
    let mut maintained: Option<NaiveDate> = None;
    loop {
        ticker.tick().await;
        let now = Utc::now();
        if maintained != Some(now.date_naive()) {
            daily_maintenance(&pool, now).await;
            maintained = Some(now.date_naive());
        }
        let readout = crate::execution::concurrency::readout(&pool).await;
        let reading = readout.map(|readout| snapshot_of(&readout));
        write_tick(&pool, now, reading, &mut last).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(moment: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(moment)
            .unwrap()
            .with_timezone(&Utc)
    }

    fn day(text: &str) -> NaiveDate {
        NaiveDate::parse_from_str(text, "%Y-%m-%d").unwrap()
    }

    async fn rows(pool: &SqlitePool) -> Vec<(String, Option<String>, i64, i64)> {
        sqlx::query_as(
            "SELECT sampled_at, project_id, in_use, slot_limit FROM slot_samples
             ORDER BY sampled_at, project_id IS NOT NULL, project_id",
        )
        .fetch_all(pool)
        .await
        .unwrap()
    }

    fn snapshot(house: i64, projects: &[(&str, i64, i64)]) -> Snapshot {
        Snapshot {
            house_in_use: house,
            house_limit: 4,
            projects: projects
                .iter()
                .map(|(id, used, limit)| (id.to_string(), *used, *limit))
                .collect(),
        }
    }

    async fn seed(pool: &SqlitePool, rows: &[(&str, Option<&str>, i64, i64)]) {
        for (moment, project, in_use, limit) in rows {
            sqlx::query(
                "INSERT INTO slot_samples (sampled_at, project_id, in_use, slot_limit)
                 VALUES (?, ?, ?, ?)",
            )
            .bind(moment)
            .bind(project)
            .bind(in_use)
            .bind(limit)
            .execute(pool)
            .await
            .unwrap();
        }
    }

    async fn daily(pool: &SqlitePool) -> Vec<(String, String, i64, f64, i64, i64, i64)> {
        sqlx::query_as(
            "SELECT day, project_id, samples, avg_in_use, max_in_use, slot_limit, at_limit_samples
             FROM slot_samples_daily ORDER BY day, project_id",
        )
        .fetch_all(pool)
        .await
        .unwrap()
    }

    async fn count(pool: &SqlitePool, table: &str) -> i64 {
        sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT COUNT(*) FROM {table}")))
            .fetch_one(pool)
            .await
            .unwrap()
    }

    /// Spec §11: the house row every tick (it is the clock); an idle project is written once
    /// when it goes idle and skipped while it stays idle; a busy one every tick.
    #[tokio::test]
    async fn the_sampler_writes_one_house_row_every_tick_and_skips_idle_projects() {
        let pool = crate::testdb::fresh_pool().await;
        let mut last = Last::new();
        let ticks = [
            (
                "2026-10-17T10:00:00Z",
                snapshot(1, &[("busy", 1, 2), ("idle", 0, 1)]),
            ),
            (
                "2026-10-17T10:01:00Z",
                snapshot(1, &[("busy", 1, 2), ("idle", 0, 1)]),
            ),
            (
                "2026-10-17T10:02:00Z",
                snapshot(0, &[("busy", 0, 2), ("idle", 0, 1)]),
            ),
            (
                "2026-10-17T10:03:00Z",
                snapshot(0, &[("busy", 0, 2), ("idle", 0, 1)]),
            ),
        ];
        for (moment, reading) in ticks {
            write_tick(&pool, at(moment), Ok(reading), &mut last).await;
        }
        let written = rows(&pool).await;
        let house: Vec<_> = written.iter().filter(|row| row.1.is_none()).collect();
        assert_eq!(house.len(), 4, "one house row per tick: {written:?}");
        let idle = written
            .iter()
            .filter(|row| row.1.as_deref() == Some("idle"))
            .count();
        assert_eq!(
            idle, 1,
            "an idle project is written when it goes idle, then skipped"
        );
        let busy: Vec<i64> = written
            .iter()
            .filter(|row| row.1.as_deref() == Some("busy"))
            .map(|row| row.2)
            .collect();
        assert_eq!(
            busy,
            [1, 1, 0],
            "busy every tick, then once when it went idle"
        );
        for row in &written {
            assert!(row.0.ends_with('Z') && row.0.len() == 20, "{row:?}");
        }
    }

    /// Spec §11: a failing readout writes nothing for that tick and leaves the idle memory as it
    /// was, so the next good tick behaves as if the bad one never happened.
    #[tokio::test]
    async fn the_sampler_survives_a_failing_readout_and_writes_nothing_for_that_tick() {
        let pool = crate::testdb::fresh_pool().await;
        let mut last = Last::new();
        let first = snapshot(0, &[("p", 0, 1)]);
        write_tick(&pool, at("2026-10-17T10:00:00Z"), Ok(first), &mut last).await;
        let remembered = last.clone();
        let written = write_tick(
            &pool,
            at("2026-10-17T10:01:00Z"),
            Err(sqlx::Error::Protocol("readout failed".into())),
            &mut last,
        )
        .await;
        assert_eq!(written, 0);
        assert_eq!(last, remembered);
        assert_eq!(rows(&pool).await.len(), 2, "only the first tick's two rows");
    }

    /// Spec §11: one row per scope for a closed day; running it again changes nothing.
    #[tokio::test]
    async fn rollup_aggregates_a_closed_day_per_scope_and_is_idempotent() {
        let pool = crate::testdb::fresh_pool().await;
        let samples = [
            ("2026-10-16T10:00:00Z", None, 4, 4),
            ("2026-10-16T10:01:00Z", None, 2, 4),
            ("2026-10-16T10:00:00Z", Some("p"), 2, 2),
            ("2026-10-17T00:00:00Z", None, 1, 4), // the next day: not rolled into the 16th
        ];
        seed(&pool, &samples).await;
        rollup_day(&pool, day("2026-10-16")).await.unwrap();
        rollup_day(&pool, day("2026-10-16")).await.unwrap();
        assert_eq!(
            daily(&pool).await,
            vec![
                ("2026-10-16".into(), "".into(), 2, 3.0, 4, 4, 1),
                ("2026-10-16".into(), "p".into(), 1, 2.0, 2, 2, 1),
            ]
        );
    }

    /// Spec §11: on start, every closed day between the last daily row and yesterday that still
    /// has raw rows is rolled up; today never is.
    #[tokio::test]
    async fn rollup_catches_up_missed_days_on_start() {
        let pool = crate::testdb::fresh_pool().await;
        let samples = [
            ("2026-10-13T10:00:00Z", None, 1, 4),
            ("2026-10-15T10:00:00Z", None, 2, 4),
            ("2026-10-17T10:00:00Z", None, 3, 4), // today
        ];
        seed(&pool, &samples).await;
        let rolled = catch_up(&pool, day("2026-10-17")).await.unwrap();
        assert_eq!(rolled, 2, "the 13th and the 15th; the 14th had no samples");
        let days: Vec<String> = daily(&pool).await.into_iter().map(|row| row.0).collect();
        assert_eq!(days, ["2026-10-13", "2026-10-15"]);
        // Started again the same day: nothing new to roll.
        assert_eq!(catch_up(&pool, day("2026-10-17")).await.unwrap(), 0);
    }

    /// Spec §11: raw rows older than 8 days go, in batches smaller than the work; newer ones stay.
    #[tokio::test]
    async fn pruning_removes_only_rows_older_than_the_retention_in_batches() {
        let pool = crate::testdb::fresh_pool().await;
        let old: Vec<String> = (0..5)
            .map(|minute| format!("2026-10-01T10:0{minute}:00Z"))
            .collect();
        for moment in &old {
            seed(&pool, &[(moment.as_str(), None, 1, 4)]).await;
        }
        // 8 days before the 17th: kept.
        seed(&pool, &[("2026-10-09T10:00:00Z", None, 1, 4)]).await;
        rollup_day(&pool, day("2026-10-01")).await.unwrap();

        let now = at("2026-10-17T12:00:00Z");
        let pruned = prune_raw(&pool, now, 2).await.unwrap();
        assert_eq!(pruned, 5, "three batches of at most two");
        assert_eq!(count(&pool, "slot_samples").await, 1);
    }

    /// Spec §11: a day with samples but no daily row is never pruned.
    #[tokio::test]
    async fn raw_pruning_never_drops_a_day_without_its_daily_row() {
        let pool = crate::testdb::fresh_pool().await;
        seed(&pool, &[("2026-10-01T10:00:00Z", None, 1, 4)]).await;
        let now = at("2026-10-17T12:00:00Z");
        assert_eq!(prune_raw(&pool, now, 100).await.unwrap(), 0);
        assert_eq!(count(&pool, "slot_samples").await, 1);
    }

    /// Spec §11 and §10: daily rows after 400 days, transitions after 365.
    #[tokio::test]
    async fn daily_rows_are_pruned_after_400_days() {
        let pool = crate::testdb::fresh_pool().await;
        for day in ["2025-09-11", "2025-09-12"] {
            sqlx::query(
                "INSERT INTO slot_samples_daily
                     (day, samples, avg_in_use, max_in_use, slot_limit, at_limit_samples)
                 VALUES (?, 1, 0, 0, 4, 0)",
            )
            .bind(day)
            .execute(&pool)
            .await
            .unwrap();
        }
        for moment in ["2025-10-16T10:00:00Z", "2025-10-18T10:00:00Z"] {
            sqlx::query(
                "INSERT INTO postgate_transitions (project_id, at, to_state)
                 VALUES ('p', ?, 'red')",
            )
            .bind(moment)
            .execute(&pool)
            .await
            .unwrap();
        }
        let now = at("2026-10-17T12:00:00Z"); // 400 days before: 2025-09-12
        assert_eq!(prune_daily(&pool, now, 1).await.unwrap(), 1);
        assert_eq!(prune_transitions(&pool, now, 1).await.unwrap(), 1);
        assert_eq!(count(&pool, "slot_samples_daily").await, 1);
        assert_eq!(count(&pool, "postgate_transitions").await, 1);
    }
}
