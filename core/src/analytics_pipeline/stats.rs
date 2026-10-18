//! Pure helpers for the pipeline blocks (spec analytics-pipeline §6).

use chrono::NaiveDate;

/// The UTC day an RFC 3339 timestamp (or a bare `YYYY-MM-DD`) falls on: its first ten characters.
pub fn day_of(at: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(at.get(..10)?, "%Y-%m-%d").ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_day_is_the_first_ten_characters_of_a_timestamp() {
        let expected = NaiveDate::from_ymd_opt(2026, 10, 17);
        assert_eq!(day_of("2026-10-17T23:59:59Z"), expected);
        assert_eq!(day_of("2026-10-17"), expected);
        assert_eq!(day_of("not a day"), None);
        assert_eq!(day_of("2026"), None);
    }
}
