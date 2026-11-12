//! Cache report (E-REPORT): reads the persisted cache observations and delegates
//! the statistical calculations to `measure`.

use chrono::{DateTime, Utc};
use sqlx::{Row, SqlitePool};

use super::measure::{self, ObservedWait, Turn};

#[derive(Debug, Clone, PartialEq)]
pub struct Figure<T> {
    pub value: T,
    pub estimated: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CacheReport {
    pub has_data: bool,
    pub ttl_distribution: measure::TtlDistribution,
    pub gap_counts: Figure<Vec<measure::BucketCount>>,
    pub miss_rates: Figure<Vec<measure::BucketRate>>,
    pub effective_ttl_s: Figure<Option<i64>>,
    pub p_resume: Option<f64>,
}

/// Builds the cache report from chat runs, main-lane devtime messages, and closed waits.
/// Timestamps are persisted as text, so a malformed row is omitted from the measurements.
pub async fn cache_report(pool: &SqlitePool) -> sqlx::Result<CacheReport> {
    let run_rows = sqlx::query("SELECT cache_ttl FROM runs WHERE chat_id IS NOT NULL")
        .fetch_all(pool)
        .await?;
    let ttls: Vec<Option<String>> = run_rows
        .iter()
        .map(|row| row.try_get("cache_ttl"))
        .collect::<sqlx::Result<_>>()?;
    let ttl_values: Vec<Option<&str>> = ttls.iter().map(Option::as_deref).collect();

    let message_rows = sqlx::query(
        "SELECT session_id, first_at, last_at, input_tokens, cache_read_tokens, \
         cache_creation_tokens FROM devtime_messages WHERE lane = 'main'",
    )
    .fetch_all(pool)
    .await?;
    let mut turns = Vec::new();
    for row in message_rows {
        let session_id: String = row.try_get("session_id")?;
        let first_at: String = row.try_get("first_at")?;
        let last_at: String = row.try_get("last_at")?;
        let (Ok(first_at), Ok(last_at)) = (
            DateTime::parse_from_rfc3339(&first_at),
            DateTime::parse_from_rfc3339(&last_at),
        ) else {
            continue;
        };
        turns.push(Turn {
            session_id,
            first_at: first_at.with_timezone(&Utc),
            last_at: last_at.with_timezone(&Utc),
            input_tokens: row.try_get("input_tokens")?,
            cache_read_tokens: row.try_get("cache_read_tokens")?,
            cache_creation_tokens: row.try_get("cache_creation_tokens")?,
        });
    }

    let wait_rows = sqlx::query(
        "SELECT expected_wait_s, returned, decided_at, returned_at \
         FROM cache_waits",
    )
    .fetch_all(pool)
    .await?;
    let mut waits = Vec::new();
    let mut has_wait_data = false;
    for row in wait_rows {
        let expected_s: i64 = row.try_get("expected_wait_s")?;
        let returned: Option<i64> = row.try_get("returned")?;
        let decided_at: String = row.try_get("decided_at")?;
        let returned_at: Option<String> = row.try_get("returned_at")?;
        let Ok(decided_at) = DateTime::parse_from_rfc3339(&decided_at) else {
            continue;
        };
        let returned_at = match returned_at {
            Some(returned_at) => match DateTime::parse_from_rfc3339(&returned_at) {
                Ok(returned_at) => Some(returned_at),
                Err(_) => continue,
            },
            None => None,
        };
        has_wait_data = true;
        let returned_after_s = match returned {
            Some(0) => None,
            Some(1) => match returned_at {
                Some(returned_at) => Some((returned_at - decided_at).num_seconds()),
                None => continue,
            },
            _ => continue,
        };
        waits.push(ObservedWait {
            expected_s,
            returned_after_s,
        });
    }

    let estimated = !turns.is_empty();
    let gap_counts = measure::gap_buckets(&turns);
    let miss_rates = measure::miss_rate_by_bucket(&gap_counts);
    let effective_ttl_s = measure::effective_ttl(&miss_rates);

    Ok(CacheReport {
        has_data: !ttls.is_empty() || !turns.is_empty() || has_wait_data,
        ttl_distribution: measure::ttl_distribution(&ttl_values),
        gap_counts: Figure {
            value: gap_counts,
            estimated,
        },
        miss_rates: Figure {
            value: miss_rates,
            estimated,
        },
        effective_ttl_s: Figure {
            value: effective_ttl_s,
            estimated,
        },
        p_resume: measure::p_resume(&waits),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    use chrono::{DateTime, Duration, TimeZone, Utc};
    use sqlx::SqlitePool;

    use crate::tree_cost::measure::{BucketCount, BucketRate, GapBucket, TtlDistribution};

    /// Every turn lasts this long, so a gap is `first_at - previous last_at`.
    const TURN_S: i64 = 30;

    const ALL_BUCKETS: [GapBucket; 6] = [
        GapBucket::Under5,
        GapBucket::From5To10,
        GapBucket::From10To30,
        GapBucket::From30To55,
        GapBucket::From55To65,
        GapBucket::Over65,
    ];

    fn at(offset_s: i64) -> String {
        let base: DateTime<Utc> = Utc.with_ymd_and_hms(2026, 10, 1, 0, 0, 0).unwrap();
        (base + Duration::seconds(offset_s)).to_rfc3339()
    }

    fn count_of(counts: &[BucketCount], bucket: GapBucket) -> (u32, u32) {
        let count = counts.iter().find(|c| c.bucket == bucket).unwrap();
        (count.total, count.misses)
    }

    fn rate_of(rates: &[BucketRate], bucket: GapBucket) -> (u32, Option<f64>) {
        let found = rates.iter().find(|r| r.bucket == bucket).unwrap();
        (found.total, found.rate)
    }

    fn assert_all_buckets_empty(counts: &[BucketCount], rates: &[BucketRate]) {
        let order: Vec<GapBucket> = counts.iter().map(|c| c.bucket).collect();
        assert_eq!(order, ALL_BUCKETS.to_vec());
        assert!(counts.iter().all(|c| c.total == 0 && c.misses == 0));

        let order: Vec<GapBucket> = rates.iter().map(|r| r.bucket).collect();
        assert_eq!(order, ALL_BUCKETS.to_vec());
        assert!(rates.iter().all(|r| r.total == 0 && r.rate.is_none()));
    }

    /// One chat turn, the only kind of run `cache_ttl` is ever written on.
    async fn insert_chat_turn(pool: &SqlitePool, cache_ttl: Option<&str>) {
        sqlx::query(
            "INSERT INTO runs (prompt, status, mode, session_id, chat_id, created_at, cache_ttl) \
             VALUES ('p', 'completed', 'assistant', 's', 'chat-1', ?, ?)",
        )
        .bind(at(0))
        .bind(cache_ttl)
        .execute(pool)
        .await
        .unwrap();
    }

    /// A run that belongs to no chat: its NULL `cache_ttl` says nothing about a granted TTL.
    async fn insert_non_chat_run(pool: &SqlitePool) {
        sqlx::query("INSERT INTO runs (prompt, status, created_at) VALUES ('p', 'completed', ?)")
            .bind(at(0))
            .execute(pool)
            .await
            .unwrap();
    }

    /// One `devtime_messages` row with a context of 1000 tokens: `cold` rewrote the cache,
    /// otherwise it read it. Any two in a row are a miss exactly when the second is cold.
    async fn insert_message(
        pool: &SqlitePool,
        session_id: &str,
        lane: &str,
        message_id: &str,
        first_s: i64,
        cold: bool,
    ) {
        let (cache_read, cache_creation) = if cold { (0, 990) } else { (990, 0) };
        sqlx::query(
            "INSERT INTO devtime_messages (session_id, lane, message_id, first_at, last_at, \
             input_tokens, cache_read_tokens, cache_creation_tokens, parser_version) \
             VALUES (?, ?, ?, ?, ?, 10, ?, ?, 1)",
        )
        .bind(session_id)
        .bind(lane)
        .bind(message_id)
        .bind(at(first_s))
        .bind(at(first_s + TURN_S))
        .bind(cache_read)
        .bind(cache_creation)
        .execute(pool)
        .await
        .unwrap();
    }

    /// A warm first message at `start_s`, then one message per `(gap_s, cold)` step, each
    /// starting `gap_s` after the previous message's `last_at`.
    async fn insert_lane(
        pool: &SqlitePool,
        session_id: &str,
        lane: &str,
        start_s: i64,
        steps: &[(i64, bool)],
    ) {
        insert_message(pool, session_id, lane, &format!("{lane}-0"), start_s, false).await;
        let mut first_s = start_s;
        for (index, &(gap_s, cold)) in steps.iter().enumerate() {
            first_s += TURN_S + gap_s;
            let message_id = format!("{lane}-{}", index + 1);
            insert_message(pool, session_id, lane, &message_id, first_s, cold).await;
        }
    }

    /// One recorded wait. `returned` NULL is a wait not closed yet; 0 closed without a return.
    async fn insert_wait(
        pool: &SqlitePool,
        expected_wait_s: i64,
        returned: Option<i64>,
        returned_after_s: Option<i64>,
    ) {
        sqlx::query(
            "INSERT INTO cache_waits (session_id, expected_wait_s, action, decided_at, returned, \
             returned_at) VALUES ('s', ?, 'park', ?, ?, ?)",
        )
        .bind(expected_wait_s)
        .bind(at(0))
        .bind(returned)
        .bind(returned_after_s.map(at))
        .execute(pool)
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn reads_runs_cache_ttl_distribution() {
        let pool = crate::testdb::fresh_pool().await;
        for ttl in [
            Some("1h"),
            Some("5m"),
            Some("1h"),
            None,
            Some("10m"),
            Some("1h"),
            None,
        ] {
            insert_chat_turn(&pool, ttl).await;
        }
        insert_non_chat_run(&pool).await;
        insert_non_chat_run(&pool).await;

        let report = cache_report(&pool).await.unwrap();

        // Spec 6.1: per chat turn, so the two non-chat runs are not two more unknowns.
        assert_eq!(
            report.ttl_distribution,
            TtlDistribution {
                one_hour: 3,
                five_minutes: 1,
                unknown: 3,
            }
        );
        assert!(report.has_data);
    }

    #[tokio::test]
    async fn reads_devtime_messages_gaps_per_session_and_lane() {
        let pool = crate::testdb::fresh_pool().await;
        // Session "b" starts inside session "a": merged, every gap below would land elsewhere.
        insert_lane(&pool, "a", "main", 0, &[(120, false), (4200, true)]).await;
        insert_lane(&pool, "b", "main", 600, &[(600, false), (3600, true)]).await;
        // A subagent speaking between the main turns of "a" splits no main-lane gap.
        insert_lane(&pool, "a", "agent:a1", 1000, &[]).await;

        let report = cache_report(&pool).await.unwrap();

        let counts = &report.gap_counts.value;
        let order: Vec<GapBucket> = counts.iter().map(|c| c.bucket).collect();
        assert_eq!(order, ALL_BUCKETS.to_vec());
        assert_eq!(count_of(counts, GapBucket::Under5), (1, 0));
        assert_eq!(count_of(counts, GapBucket::From5To10), (0, 0));
        assert_eq!(count_of(counts, GapBucket::From10To30), (1, 0));
        assert_eq!(count_of(counts, GapBucket::From30To55), (0, 0));
        assert_eq!(count_of(counts, GapBucket::From55To65), (1, 1));
        assert_eq!(count_of(counts, GapBucket::Over65), (1, 1));

        let rates = &report.miss_rates.value;
        assert_eq!(rate_of(rates, GapBucket::Under5), (1, Some(0.0)));
        assert_eq!(rate_of(rates, GapBucket::From5To10), (0, None));
        assert_eq!(rate_of(rates, GapBucket::Over65), (1, Some(1.0)));

        // One sample per bucket is far under the floor, so no TTL can be read off them.
        assert_eq!(report.effective_ttl_s.value, None);
        assert!(report.has_data);
    }

    #[tokio::test]
    async fn effective_ttl_is_read_from_the_main_lane_gaps() {
        let pool = crate::testdb::fresh_pool().await;
        let mut steps = vec![(120, false); 20];
        steps.extend(vec![(4200, true); 20]);
        insert_lane(&pool, "s", "main", 0, &steps).await;

        let report = cache_report(&pool).await.unwrap();

        assert_eq!(
            count_of(&report.gap_counts.value, GapBucket::Under5),
            (20, 0)
        );
        assert_eq!(
            count_of(&report.gap_counts.value, GapBucket::Over65),
            (20, 20)
        );
        assert_eq!(report.effective_ttl_s.value, Some(3900));
    }

    #[tokio::test]
    async fn ignores_subagent_lanes() {
        let pool = crate::testdb::fresh_pool().await;
        insert_lane(&pool, "s", "main", 0, &[(120, false)]).await;
        // In the same session as the main lane, and all of it would count as misses over 65 min.
        insert_lane(&pool, "s", "agent:a1", 40, &[(4200, true), (4200, true)]).await;
        // A lane that merely has `main` in its name is still a subagent.
        insert_lane(&pool, "other", "agent:main", 0, &[(4200, true)]).await;

        let report = cache_report(&pool).await.unwrap();

        let counts = &report.gap_counts.value;
        assert_eq!(count_of(counts, GapBucket::Under5), (1, 0));
        for bucket in [
            GapBucket::From5To10,
            GapBucket::From10To30,
            GapBucket::From30To55,
            GapBucket::From55To65,
            GapBucket::Over65,
        ] {
            assert_eq!(count_of(counts, bucket), (0, 0), "{bucket:?}");
        }

        // Subagent rows alone are no data at all.
        let only_agents = crate::testdb::fresh_pool().await;
        insert_lane(&only_agents, "s", "agent:a1", 0, &[(4200, true)]).await;

        let report = cache_report(&only_agents).await.unwrap();

        assert!(!report.has_data);
        assert_all_buckets_empty(&report.gap_counts.value, &report.miss_rates.value);
        assert!(!report.gap_counts.estimated);
    }

    #[tokio::test]
    async fn empty_database_reports_no_data() {
        let pool = crate::testdb::fresh_pool().await;

        let report = cache_report(&pool).await.unwrap();

        assert!(!report.has_data);
        assert_eq!(report.ttl_distribution, TtlDistribution::default());
        assert_all_buckets_empty(&report.gap_counts.value, &report.miss_rates.value);
        assert_eq!(report.effective_ttl_s.value, None);
        assert_eq!(report.p_resume, None);
        // Nothing was taken from `devtime_messages`, so there is nothing estimated to mark.
        assert!(!report.gap_counts.estimated);
        assert!(!report.miss_rates.estimated);
        assert!(!report.effective_ttl_s.estimated);
    }

    #[tokio::test]
    async fn p_resume_comes_from_recorded_waits() {
        let pool = crate::testdb::fresh_pool().await;
        // Returned at exactly twice the wait: counts.
        insert_wait(&pool, 600, Some(1), Some(1200)).await;
        // One second too late: qualifies, but is not a return.
        insert_wait(&pool, 600, Some(1), Some(1201)).await;
        // Closed without a return: qualifies, but is not a return.
        insert_wait(&pool, 600, Some(0), None).await;
        // Just over the 300 s floor, at exactly twice: counts.
        insert_wait(&pool, 301, Some(1), Some(602)).await;
        // At the floor: left out of the denominator.
        insert_wait(&pool, 300, Some(1), Some(10)).await;
        // Not closed yet: its outcome is unknown, so it must not read as "never returned".
        insert_wait(&pool, 600, None, None).await;
        // Devtime gaps over 5 min, every one "returned": folded in, the answer would rise.
        insert_lane(&pool, "s", "main", 0, &[(600, false); 10]).await;

        let report = cache_report(&pool).await.unwrap();

        assert_eq!(report.p_resume, Some(0.5));
    }

    /// The gap of `devtime_messages` cannot stand in for `expected_s`: a session that came back
    /// after a gap always came back within twice that same gap, so P would read about 1 whatever
    /// really happened. Without a recorded expectation the report says unknown.
    #[tokio::test]
    async fn p_resume_is_unknown_without_a_real_expected_wait() {
        let pool = crate::testdb::fresh_pool().await;
        insert_lane(&pool, "s", "main", 0, &[(600, false); 30]).await;

        let report = cache_report(&pool).await.unwrap();

        assert!(report.has_data);
        assert_eq!(report.p_resume, None);

        // Recorded waits that do not qualify, or are not closed, leave it unknown too.
        insert_wait(&pool, 300, Some(1), Some(10)).await;
        insert_wait(&pool, 600, None, None).await;

        let report = cache_report(&pool).await.unwrap();

        assert_eq!(report.p_resume, None);
    }

    #[tokio::test]
    async fn estimated_marks_only_the_figures_from_devtime_messages() {
        // The steps give the effective TTL a value, so its mark is on a real figure.
        let from_devtime = crate::testdb::fresh_pool().await;
        let mut steps = vec![(120, false); 20];
        steps.extend(vec![(4200, true); 20]);
        insert_lane(&from_devtime, "s", "main", 0, &steps).await;

        let report = cache_report(&from_devtime).await.unwrap();

        assert!(report.gap_counts.estimated);
        assert!(report.miss_rates.estimated);
        assert!(report.effective_ttl_s.estimated);

        // Real runs and recorded waits, with no devtime row: data, and nothing estimated.
        let measured = crate::testdb::fresh_pool().await;
        insert_chat_turn(&measured, Some("1h")).await;
        insert_wait(&measured, 600, Some(1), Some(700)).await;

        let report = cache_report(&measured).await.unwrap();

        assert!(report.has_data);
        assert_eq!(report.ttl_distribution.one_hour, 1);
        assert_eq!(report.p_resume, Some(1.0));
        assert!(!report.gap_counts.estimated);
        assert!(!report.miss_rates.estimated);
        assert!(!report.effective_ttl_s.estimated);
    }
}
