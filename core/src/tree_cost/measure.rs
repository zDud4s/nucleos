//! Cache measurement (E-MEASURE): pure analysis over rows already read, with no
//! behaviour change. Cache-miss rate by gap bucket, the effective TTL, P(resume)
//! and the distribution of granted TTLs. The caller (E-REPORT) reads the rows and
//! filters them to the main lane.

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, Duration, TimeZone, Utc};

    /// Every turn lasts this long, so a gap measured from `first_at` to `first_at`
    /// lands on the wrong side of the bucket edges.
    const TURN_S: i64 = 30;

    const ALL_BUCKETS: [GapBucket; 6] = [
        GapBucket::Under5,
        GapBucket::From5To10,
        GapBucket::From10To30,
        GapBucket::From30To55,
        GapBucket::From55To65,
        GapBucket::Over65,
    ];

    fn at(offset_s: i64) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 1, 0, 0, 0).unwrap() + Duration::seconds(offset_s)
    }

    /// A turn whose timestamps do not matter, for the `is_miss` cases.
    fn tokens(input: i64, cache_read: i64, cache_creation: i64) -> Turn {
        Turn {
            session_id: "s".to_string(),
            first_at: at(0),
            last_at: at(TURN_S),
            input_tokens: input,
            cache_read_tokens: cache_read,
            cache_creation_tokens: cache_creation,
        }
    }

    /// A turn with a context of 1000 tokens: `cold` rewrote the cache, otherwise it
    /// read it. Any two of these in a row are a miss exactly when the second is cold.
    fn turn(session_id: &str, first_s: i64, cold: bool) -> Turn {
        let (cache_read, cache_creation) = if cold { (0, 990) } else { (990, 0) };
        Turn {
            session_id: session_id.to_string(),
            first_at: at(first_s),
            last_at: at(first_s + TURN_S),
            input_tokens: 10,
            cache_read_tokens: cache_read,
            cache_creation_tokens: cache_creation,
        }
    }

    /// A warm first turn at `start_s`, then one turn per `(gap_s, cold)` step, each
    /// starting `gap_s` after the previous turn's `last_at`.
    fn session(session_id: &str, start_s: i64, steps: &[(i64, bool)]) -> Vec<Turn> {
        let mut turns = vec![turn(session_id, start_s, false)];
        let mut first_s = start_s;
        for &(gap_s, cold) in steps {
            first_s += TURN_S + gap_s;
            turns.push(turn(session_id, first_s, cold));
        }
        turns
    }

    fn rate(bucket: GapBucket, total: u32, rate: f64) -> BucketRate {
        BucketRate {
            bucket,
            total,
            rate: Some(rate),
        }
    }

    fn empty(bucket: GapBucket) -> BucketRate {
        BucketRate {
            bucket,
            total: 0,
            rate: None,
        }
    }

    fn count_of(counts: &[BucketCount], bucket: GapBucket) -> (u32, u32) {
        let count = counts.iter().find(|c| c.bucket == bucket).unwrap();
        (count.total, count.misses)
    }

    fn rate_of(rates: &[BucketRate], bucket: GapBucket) -> (u32, Option<f64>) {
        let found = rates.iter().find(|r| r.bucket == bucket).unwrap();
        (found.total, found.rate)
    }

    fn wait(expected_s: i64, returned_after_s: Option<i64>) -> ObservedWait {
        ObservedWait {
            expected_s,
            returned_after_s,
        }
    }

    #[test]
    fn miss_rate_jumps_after_the_ttl_bucket() {
        let turns = session(
            "s",
            0,
            &[
                // Under 5 minutes: sampled, and never a miss.
                (120, false),
                (120, false),
                // 55-65 minutes: one miss in four.
                (3600, false),
                (3600, false),
                (3600, true),
                (3600, false),
                // Over 65 minutes: three misses in four.
                (4200, true),
                (4200, true),
                (4200, false),
                (4200, true),
            ],
        );

        let rates = miss_rate_by_bucket(&gap_buckets(&turns));

        let order: Vec<GapBucket> = rates.iter().map(|r| r.bucket).collect();
        assert_eq!(order, ALL_BUCKETS.to_vec());

        let (before_total, before) = rate_of(&rates, GapBucket::From55To65);
        let (after_total, after) = rate_of(&rates, GapBucket::Over65);
        assert_eq!((before_total, before), (4, Some(0.25)));
        assert_eq!((after_total, after), (4, Some(0.75)));
        assert!(after > before, "the rate must jump after the TTL bucket");

        // Sampled with no misses is a rate of zero; only an empty bucket has no rate.
        assert_eq!(rate_of(&rates, GapBucket::Under5), (2, Some(0.0)));
        assert_eq!(rate_of(&rates, GapBucket::From10To30), (0, None));
    }

    #[test]
    fn effective_ttl_is_65_minutes_when_one_hour_cache() {
        let rates = [
            rate(GapBucket::Under5, 30, 0.0),
            rate(GapBucket::From5To10, 30, 0.02),
            rate(GapBucket::From10To30, 30, 0.05),
            rate(GapBucket::From30To55, 25, 0.1),
            rate(GapBucket::From55To65, 20, 0.2),
            rate(GapBucket::Over65, 20, 0.9),
        ];

        assert_eq!(effective_ttl(&rates), Some(3900));
    }

    #[test]
    fn effective_ttl_is_none_without_enough_samples() {
        // The jump bucket is one sample short.
        let jump_undersampled = [
            rate(GapBucket::Under5, 30, 0.0),
            rate(GapBucket::From5To10, 30, 0.0),
            rate(GapBucket::From10To30, 30, 0.0),
            rate(GapBucket::From30To55, 30, 0.0),
            rate(GapBucket::From55To65, 30, 0.0),
            rate(GapBucket::Over65, MIN_BUCKET_SAMPLES - 1, 0.9),
        ];
        assert_eq!(effective_ttl(&jump_undersampled), None);

        // High from the first sampled bucket on: no low bucket to jump from.
        let always_high = [
            rate(GapBucket::Under5, 30, 0.9),
            rate(GapBucket::From5To10, 30, 0.9),
            empty(GapBucket::From10To30),
            empty(GapBucket::From30To55),
            empty(GapBucket::From55To65),
            rate(GapBucket::Over65, 30, 0.9),
        ];
        assert_eq!(effective_ttl(&always_high), None);

        // The only low bucket before the jump is one sample short.
        let low_undersampled = [
            rate(GapBucket::Under5, MIN_BUCKET_SAMPLES - 1, 0.0),
            empty(GapBucket::From5To10),
            empty(GapBucket::From10To30),
            empty(GapBucket::From30To55),
            empty(GapBucket::From55To65),
            rate(GapBucket::Over65, 30, 0.9),
        ];
        assert_eq!(effective_ttl(&low_undersampled), None);

        // A low bucket after the high one does not make the earlier one a jump.
        let low_only_after_high = [
            rate(GapBucket::Under5, 30, 0.9),
            rate(GapBucket::From5To10, 30, 0.0),
            empty(GapBucket::From10To30),
            empty(GapBucket::From30To55),
            empty(GapBucket::From55To65),
            empty(GapBucket::Over65),
        ];
        assert_eq!(effective_ttl(&low_only_after_high), None);
    }

    #[test]
    fn effective_ttl_is_five_minutes_when_rate_jumps_early() {
        // Exactly the minimum samples and exactly the jump rate both qualify, and
        // the scan stops at the first such bucket.
        let rates = [
            rate(GapBucket::Under5, MIN_BUCKET_SAMPLES, 0.0),
            rate(GapBucket::From5To10, MIN_BUCKET_SAMPLES, JUMP_MISS_RATE),
            rate(GapBucket::From10To30, 30, 0.9),
            rate(GapBucket::From30To55, 30, 0.9),
            rate(GapBucket::From55To65, 30, 0.9),
            rate(GapBucket::Over65, 30, 0.9),
        ];

        assert_eq!(effective_ttl(&rates), Some(300));
    }

    #[test]
    fn p_resume_counts_returns_within_twice_the_gap() {
        let waits = [
            // Returned at exactly twice the wait: counts.
            wait(600, Some(1200)),
            // One second too late: qualifies, but is not a return.
            wait(600, Some(1201)),
            // Never returned: qualifies, but is not a return.
            wait(600, None),
            // Just over the 300 s floor, at exactly twice: counts.
            wait(301, Some(602)),
            // Exactly 300 s does not qualify, so it is left out of the denominator.
            wait(300, Some(10)),
        ];
        assert_eq!(p_resume(&waits), Some(0.5));

        assert_eq!(p_resume(&[]), None);
        assert_eq!(p_resume(&[wait(300, Some(10)), wait(120, None)]), None);
    }

    #[test]
    fn ttl_distribution_counts_1h_and_5m() {
        let ttls = [
            Some("1h"),
            Some("5m"),
            Some("1h"),
            None,
            Some("10m"),
            Some(""),
            Some("1H"),
            Some("1h"),
        ];
        assert_eq!(
            ttl_distribution(&ttls),
            TtlDistribution {
                one_hour: 3,
                five_minutes: 1,
                unknown: 4,
            }
        );

        assert_eq!(
            ttl_distribution(&[]),
            TtlDistribution {
                one_hour: 0,
                five_minutes: 0,
                unknown: 0,
            }
        );
    }

    #[test]
    fn miss_requires_creation_at_least_half_of_previous_context() {
        // Context 1000, with all three token kinds contributing: dropping any one of
        // them moves the half and flips the "creation exactly half" case below.
        let prev = tokens(100, 200, 700);

        // Low read with low creation: not a miss.
        assert!(!is_miss(&prev, &tokens(0, 100, 100)));
        // Low read with creation one short of half: not a miss.
        assert!(!is_miss(&prev, &tokens(0, 0, 499)));
        // Low read with creation exactly half: a miss.
        assert!(is_miss(&prev, &tokens(0, 499, 500)));
        // Read exactly half: not a miss, however much was created.
        assert!(!is_miss(&prev, &tokens(0, 500, 900)));
        // A healthy read: not a miss.
        assert!(!is_miss(&prev, &tokens(0, 990, 10)));

        // An odd context of 1001 has a half of 500.5, which integer division loses.
        let odd = tokens(1, 300, 700);
        assert!(is_miss(&odd, &tokens(0, 500, 501)));
        assert!(!is_miss(&odd, &tokens(0, 500, 500)));
    }

    #[test]
    fn gap_buckets_cuts_at_lower_edges_and_returns_all_six() {
        let turns = session(
            "s",
            0,
            &[
                (0, false),
                (299, true),
                (300, false),
                (599, false),
                (600, false),
                (1799, false),
                (1800, false),
                (3299, false),
                (3300, false),
                (3899, false),
                (3900, true),
                (7200, true),
            ],
        );

        let counts = gap_buckets(&turns);

        let order: Vec<GapBucket> = counts.iter().map(|c| c.bucket).collect();
        assert_eq!(order, ALL_BUCKETS.to_vec());
        assert_eq!(count_of(&counts, GapBucket::Under5), (2, 1));
        assert_eq!(count_of(&counts, GapBucket::From5To10), (2, 0));
        assert_eq!(count_of(&counts, GapBucket::From10To30), (2, 0));
        assert_eq!(count_of(&counts, GapBucket::From30To55), (2, 0));
        assert_eq!(count_of(&counts, GapBucket::From55To65), (2, 0));
        assert_eq!(count_of(&counts, GapBucket::Over65), (2, 2));

        // No input, and a lone first turn with no gap: still all six, all empty.
        let lone = session("lone", 0, &[]);
        for counts in [gap_buckets(&[]), gap_buckets(&lone)] {
            let order: Vec<GapBucket> = counts.iter().map(|c| c.bucket).collect();
            assert_eq!(order, ALL_BUCKETS.to_vec());
            assert!(counts.iter().all(|c| c.total == 0 && c.misses == 0));
        }
    }

    #[test]
    fn gap_buckets_sorts_and_separates_sessions() {
        // Session "a" spans session "b" in time. Merged across sessions the gaps
        // would fall in other buckets, and unsorted the miss would be lost, because
        // the cold turn of "a" would come first.
        let mut a = session("a", 0, &[(4200, true)]);
        let mut b = session("b", 600, &[(120, false)]);
        let mut turns = Vec::new();
        turns.append(&mut a);
        turns.append(&mut b);
        turns.reverse();

        let counts = gap_buckets(&turns);

        assert_eq!(count_of(&counts, GapBucket::Under5), (1, 0));
        assert_eq!(count_of(&counts, GapBucket::From5To10), (0, 0));
        assert_eq!(count_of(&counts, GapBucket::From10To30), (0, 0));
        assert_eq!(count_of(&counts, GapBucket::From30To55), (0, 0));
        assert_eq!(count_of(&counts, GapBucket::From55To65), (0, 0));
        assert_eq!(count_of(&counts, GapBucket::Over65), (1, 1));
    }
}
