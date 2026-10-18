//! Pure arithmetic shared by every block (spec analytics-pipeline D3, §6.1): nearest-rank
//! percentiles in Rust (SQLite has none), ratios that are `None` rather than 0 or NaN on an empty
//! denominator, and the timestamp reading every table here needs.

use chrono::{DateTime, NaiveDate, NaiveDateTime, Utc};
use serde::Serialize;

/// p50/p95 and how many values they were taken over. Serialised as `{"p50", "p95", "n"}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Dist {
    pub p50: Option<i64>,
    pub p95: Option<i64>,
    pub n: i64,
}

/// Below this many values the p95 is omitted rather than pretending to a precision it lacks.
pub const MIN_FOR_P95: usize = 5;

/// Nearest rank over values already sorted ascending: `sorted[ceil(p*n) - 1]`.
pub fn percentile(sorted: &[i64], p: f64) -> Option<i64> {
    if sorted.is_empty() {
        return None;
    }
    let rank = (p * sorted.len() as f64).ceil() as usize;
    sorted
        .get(rank.saturating_sub(1).min(sorted.len() - 1))
        .copied()
}

/// p50 over any non-empty set, p95 from five values up. Global figures are computed over all
/// the rows, never as an average of per-project percentiles (spec §6.1).
pub fn dist(mut values: Vec<i64>) -> Dist {
    values.sort_unstable();
    let n = values.len();
    Dist {
        p50: percentile(&values, 0.5),
        p95: if n >= MIN_FOR_P95 {
            percentile(&values, 0.95)
        } else {
            None
        },
        n: n as i64,
    }
}

pub fn ratio(numerator: i64, denominator: i64) -> Option<f64> {
    (denominator > 0).then(|| numerator as f64 / denominator as f64)
}

/// An instant as the tables write it: chrono's `to_rfc3339()` (vcs, jobs, runs, transitions) or
/// SQLite's `CURRENT_TIMESTAMP`, `YYYY-MM-DD HH:MM:SS` in UTC (`postgate_state`, sqlx's own table).
pub fn parse_at(at: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(at)
        .map(|moment| moment.with_timezone(&Utc))
        .ok()
        .or_else(|| {
            NaiveDateTime::parse_from_str(at, "%Y-%m-%d %H:%M:%S")
                .ok()
                .map(|naive| naive.and_utc())
        })
}

/// Whole seconds from `from` to `to`; `None` when either does not parse or time runs backwards.
pub fn secs_between(from: &str, to: &str) -> Option<i64> {
    let seconds = (parse_at(to)? - parse_at(from)?).num_seconds();
    (seconds >= 0).then_some(seconds)
}

/// The UTC day an RFC 3339 timestamp (or a bare `YYYY-MM-DD`) falls on: its first ten characters.
pub fn day_of(at: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(at.get(..10)?, "%Y-%m-%d").ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Spec §11 and D3: `idx = ceil(p*n) - 1` over the sorted values; no p95 under five samples.
    #[test]
    fn percentile_uses_nearest_rank_and_omits_p95_below_five_samples() {
        let ten: Vec<i64> = (1..=10).collect();
        assert_eq!(percentile(&ten, 0.5), Some(5));
        assert_eq!(percentile(&ten, 0.95), Some(10));
        assert_eq!(percentile(&[], 0.5), None);

        let four = Dist {
            p50: Some(2),
            p95: None,
            n: 4,
        };
        assert_eq!(dist(vec![4, 1, 3, 2]), four);
        let five = Dist {
            p50: Some(3),
            p95: Some(5),
            n: 5,
        };
        assert_eq!(dist(vec![5, 1, 4, 2, 3]), five);
        let none = Dist {
            p50: None,
            p95: None,
            n: 0,
        };
        assert_eq!(dist(vec![]), none);
    }

    /// Spec §11 and §6.1: a ratio over nothing is absent, never 0 and never NaN.
    #[test]
    fn a_ratio_with_a_zero_denominator_is_none_not_zero() {
        assert_eq!(ratio(0, 0), None);
        assert_eq!(ratio(3, 0), None);
        assert_eq!(ratio(0, 5), Some(0.0));
        assert_eq!(ratio(1, 4), Some(0.25));
    }

    /// Both spellings the tables hold: chrono's RFC 3339 and SQLite's `CURRENT_TIMESTAMP`.
    #[test]
    fn timestamps_read_in_both_spellings_and_a_backwards_interval_is_absent() {
        let rfc = "2026-10-17T10:00:00+00:00";
        assert_eq!(secs_between(rfc, "2026-10-17T10:00:12.5Z"), Some(12));
        assert_eq!(
            secs_between("2026-10-17 10:00:00", "2026-10-17T10:01:00Z"),
            Some(60)
        );
        assert_eq!(
            secs_between("2026-10-17T10:01:00Z", "2026-10-17T10:00:00Z"),
            None
        );
        assert_eq!(secs_between("garbage", "2026-10-17T10:00:00Z"), None);
        assert_eq!(
            day_of("2026-10-17T23:59:59Z"),
            NaiveDate::from_ymd_opt(2026, 10, 17)
        );
        assert_eq!(day_of("17/10"), None);
    }

    #[test]
    fn a_day_is_the_first_ten_characters_of_a_timestamp() {
        let expected = NaiveDate::from_ymd_opt(2026, 10, 17);
        assert_eq!(day_of("2026-10-17T23:59:59Z"), expected);
        assert_eq!(day_of("2026-10-17"), expected);
        assert_eq!(day_of("not a day"), None);
        assert_eq!(day_of("2026"), None);
    }
}
