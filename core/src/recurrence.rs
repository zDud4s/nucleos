//! Pure recurrence expansion: a rule and a window in, the occurrences that fall inside it out.
//!
//! Nothing here reads a clock, opens a database or knows what an event means. That is the whole
//! point of the file existing separately from `calendar.rs`: every genuinely hard question a
//! calendar has — what happens to a weekly 09:00 when the clocks change, which Thursday is "the
//! third one" in a month that starts on a Friday, what a cancelled occurrence does to the ones
//! after it — becomes a table test with no infrastructure. It is the same split `classifier.rs`
//! has from `hooks.rs`, and `job.rs::next_step` has from the rest of `job.rs`, for the same reason.
//!
//! The supported subset is daily/weekly/monthly with an interval, `BYDAY`, and either a `COUNT` or
//! an `UNTIL`. This is not RFC 5545 and does not pretend to be: `BYSETPOS`, `BYYEARDAY` and `WKST`
//! are absent because a personal calendar has never needed them. If importing a third party's
//! `.ics` ever becomes a goal, this file is the one that gets replaced by a real RRULE crate — the
//! boundary is drawn so that nothing else would have to change.

use std::collections::BTreeMap;

use chrono::{
    DateTime, Datelike, Duration, LocalResult, NaiveDate, NaiveDateTime, TimeZone, Timelike, Utc,
    Weekday,
};
use chrono_tz::Tz;

/// How far the expander will walk before giving up.
///
/// `COUNT` is defined from the start of the series, not from the start of the query window, so an
/// old daily series has to be walked to be counted correctly. That walk is bounded rather than
/// clever: a daily rule running since 1970 reaches roughly 20 000 steps, so this ceiling is two
/// orders of magnitude above any real calendar while still making non-termination impossible.
const MAX_BLOCKS: i64 = 100_000;

/// The longest gap a spring-forward transition can open, in minutes.
///
/// Real transitions are 30 or 60 minutes; Lord Howe Island's is 30. Three hours is slack, and
/// bounding the search is what stops a corrupt zone from turning [`resolve`] into a loop.
const MAX_GAP_MINUTES: i64 = 180;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Freq {
    Daily,
    Weekly,
    Monthly,
}

impl Freq {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "daily" => Some(Self::Daily),
            "weekly" => Some(Self::Weekly),
            "monthly" => Some(Self::Monthly),
            _ => None,
        }
    }
}

/// A weekday selector inside a rule.
///
/// `Every` is what a weekly rule uses ("Mondays and Wednesdays"); `Nth` is what a monthly one uses
/// ("the third Thursday", or with a negative index, "the last Friday").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ByDay {
    Every(Weekday),
    Nth(Weekday, i8),
}

/// Parses the stored `byday` column: `MO,WE,FR` for a weekly rule, `TH#3` or `FR#-1` for a monthly
/// one.
///
/// An unparseable entry is dropped rather than failing the whole rule. A single bad token must not
/// silence a calendar — the same isolation principle the email sidecar's `Cycle` applies per
/// mailbox.
pub fn parse_byday(raw: &str) -> Vec<ByDay> {
    raw.split(',')
        .filter_map(|token| {
            let token = token.trim();
            if token.is_empty() {
                return None;
            }
            let (day, nth) = match token.split_once('#') {
                Some((day, nth)) => (day, Some(nth.parse::<i8>().ok()?)),
                None => (token, None),
            };
            let weekday = match day.to_ascii_uppercase().as_str() {
                "MO" => Weekday::Mon,
                "TU" => Weekday::Tue,
                "WE" => Weekday::Wed,
                "TH" => Weekday::Thu,
                "FR" => Weekday::Fri,
                "SA" => Weekday::Sat,
                "SU" => Weekday::Sun,
                _ => return None,
            };
            Some(match nth {
                // `#0` has no meaning in the subset and is treated as unqualified rather than
                // rejected, because dropping the token entirely would silently widen the rule.
                Some(0) | None => ByDay::Every(weekday),
                Some(nth) => ByDay::Nth(weekday, nth),
            })
        })
        .collect()
}

/// A recurrence rule as the database holds it, already parsed.
///
/// `freq: None` is a one-off event, which is the common case: the other fields are then ignored.
#[derive(Debug, Clone)]
pub struct Rule {
    pub starts_at_local: NaiveDateTime,
    pub duration_minutes: i64,
    pub tz: Tz,
    pub freq: Option<Freq>,
    pub interval: u32,
    pub byday: Vec<ByDay>,
    pub until_local: Option<NaiveDateTime>,
    pub count: Option<u32>,
}

/// What was done to one occurrence of a series.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Exception {
    Cancelled,
    Moved {
        to_local: NaiveDateTime,
        duration_minutes: i64,
    },
}

/// One resolved occurrence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Occurrence {
    /// The occurrence's ORIGINAL local start, before any move.
    ///
    /// This is the identity an exception is keyed by, so it must survive being moved — otherwise
    /// moving an occurrence twice would create a second one rather than relocating the first.
    pub occurrence_local: NaiveDateTime,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
}

/// Turns a local wall-clock time into an instant, deciding the two cases where that is not a
/// function.
///
/// Twice a year a local time is either impossible or means two different instants, and a calendar
/// that has not decided what to do about it decides by accident:
///
/// - **Gap (spring forward).** 02:30 does not happen. Advancing to the first valid instant keeps
///   the commitment; skipping the occurrence would delete an appointment in silence, which is the
///   worst shape a wrong answer can take here.
/// - **Fold (autumn back).** 02:30 happens twice. Taking the first is deterministic and is the one
///   the person lives through first.
fn resolve(local: NaiveDateTime, tz: Tz) -> Option<DateTime<Utc>> {
    match tz.from_local_datetime(&local) {
        LocalResult::Single(at) => Some(at.with_timezone(&Utc)),
        LocalResult::Ambiguous(first, _second) => Some(first.with_timezone(&Utc)),
        LocalResult::None => {
            for minute in 1..=MAX_GAP_MINUTES {
                let shifted = local + Duration::minutes(minute);
                match tz.from_local_datetime(&shifted) {
                    LocalResult::Single(at) => return Some(at.with_timezone(&Utc)),
                    LocalResult::Ambiguous(first, _) => return Some(first.with_timezone(&Utc)),
                    LocalResult::None => continue,
                }
            }
            None
        }
    }
}

fn week_start(date: NaiveDate) -> NaiveDate {
    date - Duration::days(date.weekday().num_days_from_monday() as i64)
}

/// Adds whole months, keeping the day of month.
///
/// Returns `None` when the target month has no such day — 31 January plus one month. That `None`
/// is the feature: the month is skipped, which is what every calendar does and what a person
/// means by "monthly on the 31st".
fn add_months(date: NaiveDate, months: i64) -> Option<NaiveDate> {
    let total = date.year() as i64 * 12 + date.month0() as i64 + months;
    let year = i32::try_from(total.div_euclid(12)).ok()?;
    let month0 = total.rem_euclid(12) as u32;
    NaiveDate::from_ymd_opt(year, month0 + 1, date.day())
}

fn first_of_month(date: NaiveDate, months: i64) -> Option<NaiveDate> {
    let total = date.year() as i64 * 12 + date.month0() as i64 + months;
    let year = i32::try_from(total.div_euclid(12)).ok()?;
    let month0 = total.rem_euclid(12) as u32;
    NaiveDate::from_ymd_opt(year, month0 + 1, 1)
}

/// The `nth` occurrence of `weekday` in the month containing `anchor`. Negative counts from the
/// end, so `-1` is the last one.
fn nth_weekday(anchor: NaiveDate, weekday: Weekday, nth: i8) -> Option<NaiveDate> {
    let first = NaiveDate::from_ymd_opt(anchor.year(), anchor.month(), 1)?;
    let offset = (7 + weekday.num_days_from_monday() as i64
        - first.weekday().num_days_from_monday() as i64)
        % 7;
    let first_match = first + Duration::days(offset);

    if nth > 0 {
        let candidate = first_match + Duration::weeks(i64::from(nth) - 1);
        (candidate.month() == anchor.month()).then_some(candidate)
    } else if nth < 0 {
        let mut candidate = first_match;
        loop {
            let next = candidate + Duration::weeks(1);
            if next.month() != anchor.month() {
                break;
            }
            candidate = next;
        }
        // `-1` is the last, `-2` the one before it.
        let candidate = candidate + Duration::weeks(i64::from(nth) + 1);
        (candidate.month() == anchor.month()).then_some(candidate)
    } else {
        None
    }
}

/// The local starts produced by one repetition unit of the rule.
///
/// A "block" is one turn of the rule's own wheel: a day for `Daily`, a week for `Weekly`, a month
/// for `Monthly`. Splitting generation this way is what lets `BYDAY` produce several occurrences
/// from a single turn without the caller needing to know it did.
fn block_occurrences(rule: &Rule, block: i64) -> Vec<NaiveDateTime> {
    let interval = i64::from(rule.interval.max(1));
    let time = rule.starts_at_local.time();
    let start_date = rule.starts_at_local.date();

    let dates: Vec<NaiveDate> = match rule.freq {
        None => {
            if block == 0 {
                vec![start_date]
            } else {
                vec![]
            }
        }
        Some(Freq::Daily) => vec![start_date + Duration::days(block * interval)],
        Some(Freq::Weekly) => {
            let base = week_start(start_date) + Duration::weeks(block * interval);
            let weekdays: Vec<Weekday> = if rule.byday.is_empty() {
                vec![start_date.weekday()]
            } else {
                rule.byday
                    .iter()
                    .map(|entry| match entry {
                        ByDay::Every(weekday) | ByDay::Nth(weekday, _) => *weekday,
                    })
                    .collect()
            };
            let mut dates: Vec<NaiveDate> = weekdays
                .into_iter()
                .map(|weekday| base + Duration::days(weekday.num_days_from_monday() as i64))
                .collect();
            dates.sort_unstable();
            dates.dedup();
            dates
        }
        Some(Freq::Monthly) => {
            let positional: Vec<(Weekday, i8)> = rule
                .byday
                .iter()
                .filter_map(|entry| match entry {
                    ByDay::Nth(weekday, nth) => Some((*weekday, *nth)),
                    ByDay::Every(_) => None,
                })
                .collect();
            if positional.is_empty() {
                add_months(start_date, block * interval)
                    .into_iter()
                    .collect()
            } else {
                let Some(anchor) = first_of_month(start_date, block * interval) else {
                    return vec![];
                };
                let mut dates: Vec<NaiveDate> = positional
                    .into_iter()
                    .filter_map(|(weekday, nth)| nth_weekday(anchor, weekday, nth))
                    .collect();
                dates.sort_unstable();
                dates.dedup();
                dates
            }
        }
    };

    dates
        .into_iter()
        // An occurrence before the series began is not an occurrence. A weekly rule anchored on a
        // Wednesday but listing Monday would otherwise emit the Monday of the starting week, three
        // days before the series exists.
        .filter(|date| *date >= start_date)
        .filter_map(|date| date.and_hms_opt(time.hour(), time.minute(), time.second()))
        .collect()
}

/// Expands `rule` into the occurrences overlapping `[from, to)`.
///
/// Overlap, not containment: a meeting that started before the window and is still running is the
/// single most important thing this function has to report, since the caller's real question is
/// "am I busy right now".
pub fn expand(
    rule: &Rule,
    exceptions: &BTreeMap<NaiveDateTime, Exception>,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> Vec<Occurrence> {
    let mut found = Vec::new();
    let mut emitted: u32 = 0;

    for block in 0..MAX_BLOCKS {
        let candidates = block_occurrences(rule, block);
        if rule.freq.is_none() && block > 0 {
            break;
        }
        // A monthly rule skips months that have no 31st, so an empty block is normal and must not
        // be read as the end of the series.
        let mut past_window = false;

        for original_local in candidates {
            if rule.until_local.is_some_and(|until| original_local > until) {
                return found;
            }
            if rule.count.is_some_and(|count| emitted >= count) {
                return found;
            }
            emitted += 1;

            // The count is spent whether or not the occurrence survives: a cancelled occurrence
            // still used up one of the series' N. Deciding otherwise would silently lengthen a
            // 10-occurrence series every time one was cancelled.
            let (local_start, duration) = match exceptions.get(&original_local) {
                Some(Exception::Cancelled) => continue,
                Some(Exception::Moved {
                    to_local,
                    duration_minutes,
                }) => (*to_local, *duration_minutes),
                None => (original_local, rule.duration_minutes),
            };

            let Some(start) = resolve(local_start, rule.tz) else {
                continue;
            };
            let end = start + Duration::minutes(duration.max(0));

            if start >= to {
                // A moved occurrence can land anywhere, so one candidate past the window does not
                // prove the series is. Only an unmoved one does.
                if local_start == original_local {
                    past_window = true;
                }
                continue;
            }
            if end > from {
                found.push(Occurrence {
                    occurrence_local: original_local,
                    start,
                    end,
                });
            }
        }

        if past_window {
            break;
        }
    }

    found
}

#[cfg(test)]
mod tests {
    use super::*;

    const LISBON: Tz = chrono_tz::Europe::Lisbon;

    fn local(text: &str) -> NaiveDateTime {
        NaiveDateTime::parse_from_str(text, "%Y-%m-%dT%H:%M:%S").expect("a valid local timestamp")
    }

    fn utc(text: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(text)
            .expect("a valid instant")
            .with_timezone(&Utc)
    }

    fn rule(starts: &str, freq: Option<Freq>) -> Rule {
        Rule {
            starts_at_local: local(starts),
            duration_minutes: 60,
            tz: LISBON,
            freq,
            interval: 1,
            byday: vec![],
            until_local: None,
            count: None,
        }
    }

    fn starts(occurrences: &[Occurrence]) -> Vec<String> {
        occurrences
            .iter()
            .map(|occurrence| occurrence.occurrence_local.to_string())
            .collect()
    }

    #[test]
    fn a_one_off_event_produces_exactly_one_occurrence() {
        let found = expand(
            &rule("2026-08-03T09:00:00", None),
            &BTreeMap::new(),
            utc("2026-08-01T00:00:00Z"),
            utc("2026-08-31T00:00:00Z"),
        );

        assert_eq!(starts(&found), vec!["2026-08-03 09:00:00"]);
    }

    #[test]
    fn a_one_off_event_outside_the_window_produces_nothing() {
        let found = expand(
            &rule("2026-09-03T09:00:00", None),
            &BTreeMap::new(),
            utc("2026-08-01T00:00:00Z"),
            utc("2026-08-31T00:00:00Z"),
        );

        assert!(found.is_empty());
    }

    /// The question the whole pillar exists to answer is "am I busy NOW", and now is usually in
    /// the middle of a meeting rather than at its start.
    #[test]
    fn an_event_already_under_way_when_the_window_opens_is_reported() {
        let found = expand(
            &rule("2026-08-03T09:00:00", None),
            &BTreeMap::new(),
            utc("2026-08-03T08:30:00Z"),
            utc("2026-08-03T08:35:00Z"),
        );

        assert_eq!(
            starts(&found),
            vec!["2026-08-03 09:00:00"],
            "Lisbon is UTC+1 in August, so 09:00 local is 08:00Z and is still running at 08:30Z"
        );
    }

    #[test]
    fn an_event_that_ended_before_the_window_is_not_reported() {
        let found = expand(
            &rule("2026-08-03T09:00:00", None),
            &BTreeMap::new(),
            utc("2026-08-03T09:00:00Z"),
            utc("2026-08-03T10:00:00Z"),
        );

        assert!(
            found.is_empty(),
            "09:00 local = 08:00Z, so it ended at 09:00Z and the window opens exactly then"
        );
    }

    #[test]
    fn a_daily_rule_repeats_every_day() {
        let found = expand(
            &rule("2026-08-03T09:00:00", Some(Freq::Daily)),
            &BTreeMap::new(),
            utc("2026-08-01T00:00:00Z"),
            utc("2026-08-06T00:00:00Z"),
        );

        assert_eq!(
            starts(&found),
            vec![
                "2026-08-03 09:00:00",
                "2026-08-04 09:00:00",
                "2026-08-05 09:00:00",
            ]
        );
    }

    #[test]
    fn an_interval_skips_the_days_between() {
        let mut every_third = rule("2026-08-03T09:00:00", Some(Freq::Daily));
        every_third.interval = 3;

        let found = expand(
            &every_third,
            &BTreeMap::new(),
            utc("2026-08-01T00:00:00Z"),
            utc("2026-08-12T00:00:00Z"),
        );

        assert_eq!(
            starts(&found),
            vec![
                "2026-08-03 09:00:00",
                "2026-08-06 09:00:00",
                "2026-08-09 09:00:00",
            ]
        );
    }

    /// An interval of zero would step nowhere and loop until the block ceiling. Clamping rather
    /// than rejecting keeps a bad row from taking the calendar down with it.
    #[test]
    fn an_interval_of_zero_behaves_as_one_rather_than_looping() {
        let mut broken = rule("2026-08-03T09:00:00", Some(Freq::Daily));
        broken.interval = 0;

        let found = expand(
            &broken,
            &BTreeMap::new(),
            utc("2026-08-03T00:00:00Z"),
            utc("2026-08-06T00:00:00Z"),
        );

        assert_eq!(found.len(), 3);
    }

    #[test]
    fn a_weekly_rule_with_byday_fires_on_each_listed_day() {
        let mut weekly = rule("2026-08-03T09:00:00", Some(Freq::Weekly));
        weekly.byday = parse_byday("MO,WE,FR");

        let found = expand(
            &weekly,
            &BTreeMap::new(),
            utc("2026-08-01T00:00:00Z"),
            utc("2026-08-10T00:00:00Z"),
        );

        assert_eq!(
            starts(&found),
            vec![
                "2026-08-03 09:00:00",
                "2026-08-05 09:00:00",
                "2026-08-07 09:00:00",
            ],
            "3 August 2026 is a Monday"
        );
    }

    /// A weekly rule anchored mid-week must not emit the listed days that fall before it began.
    #[test]
    fn a_weekly_rule_does_not_reach_back_before_its_own_start() {
        let mut weekly = rule("2026-08-05T09:00:00", Some(Freq::Weekly));
        weekly.byday = parse_byday("MO,WE");

        let found = expand(
            &weekly,
            &BTreeMap::new(),
            utc("2026-08-01T00:00:00Z"),
            utc("2026-08-13T00:00:00Z"),
        );

        assert_eq!(
            starts(&found),
            vec![
                "2026-08-05 09:00:00",
                "2026-08-10 09:00:00",
                "2026-08-12 09:00:00",
            ],
            "Monday 3 August is in the starting week but before the series began, so it is absent"
        );
    }

    #[test]
    fn a_monthly_rule_keeps_the_day_of_month() {
        let found = expand(
            &rule("2026-08-15T09:00:00", Some(Freq::Monthly)),
            &BTreeMap::new(),
            utc("2026-08-01T00:00:00Z"),
            utc("2026-11-01T00:00:00Z"),
        );

        assert_eq!(
            starts(&found),
            vec![
                "2026-08-15 09:00:00",
                "2026-09-15 09:00:00",
                "2026-10-15 09:00:00",
            ]
        );
    }

    /// What "monthly on the 31st" means in February. Skipping is what every calendar does, and the
    /// series must survive the skip rather than stop at it.
    #[test]
    fn a_monthly_rule_skips_months_without_that_day_and_continues_after() {
        let found = expand(
            &rule("2026-01-31T09:00:00", Some(Freq::Monthly)),
            &BTreeMap::new(),
            utc("2026-01-01T00:00:00Z"),
            utc("2026-05-01T00:00:00Z"),
        );

        assert_eq!(
            starts(&found),
            vec![
                "2026-01-31 09:00:00",
                "2026-03-31 09:00:00",
                // February has no 31st; the series resumes rather than ending.
            ],
            "April has 30 days, so only January and March qualify in this window"
        );
    }

    #[test]
    fn a_monthly_rule_can_name_the_third_thursday() {
        let mut monthly = rule("2026-08-20T09:00:00", Some(Freq::Monthly));
        monthly.byday = parse_byday("TH#3");

        let found = expand(
            &monthly,
            &BTreeMap::new(),
            utc("2026-08-01T00:00:00Z"),
            utc("2026-11-01T00:00:00Z"),
        );

        assert_eq!(
            starts(&found),
            vec![
                "2026-08-20 09:00:00",
                "2026-09-17 09:00:00",
                "2026-10-15 09:00:00",
            ]
        );
    }

    #[test]
    fn a_negative_position_means_the_last_such_weekday_of_the_month() {
        let mut monthly = rule("2026-08-28T09:00:00", Some(Freq::Monthly));
        monthly.byday = parse_byday("FR#-1");

        let found = expand(
            &monthly,
            &BTreeMap::new(),
            utc("2026-08-01T00:00:00Z"),
            utc("2026-10-01T00:00:00Z"),
        );

        assert_eq!(
            starts(&found),
            vec!["2026-08-28 09:00:00", "2026-09-25 09:00:00"]
        );
    }

    #[test]
    fn count_bounds_the_series() {
        let mut bounded = rule("2026-08-03T09:00:00", Some(Freq::Daily));
        bounded.count = Some(2);

        let found = expand(
            &bounded,
            &BTreeMap::new(),
            utc("2026-08-01T00:00:00Z"),
            utc("2026-09-01T00:00:00Z"),
        );

        assert_eq!(
            starts(&found),
            vec!["2026-08-03 09:00:00", "2026-08-04 09:00:00"]
        );
    }

    #[test]
    fn until_bounds_the_series_inclusively() {
        let mut bounded = rule("2026-08-03T09:00:00", Some(Freq::Daily));
        bounded.until_local = Some(local("2026-08-05T09:00:00"));

        let found = expand(
            &bounded,
            &BTreeMap::new(),
            utc("2026-08-01T00:00:00Z"),
            utc("2026-09-01T00:00:00Z"),
        );

        assert_eq!(
            starts(&found),
            vec![
                "2026-08-03 09:00:00",
                "2026-08-04 09:00:00",
                "2026-08-05 09:00:00",
            ]
        );
    }

    /// `COUNT` is defined from the start of the series. A window that opens later must not reset
    /// it, or a bounded series becomes unbounded the moment you stop looking from the beginning.
    #[test]
    fn count_is_spent_from_the_series_start_not_from_the_window() {
        let mut bounded = rule("2026-08-03T09:00:00", Some(Freq::Daily));
        bounded.count = Some(3);

        let found = expand(
            &bounded,
            &BTreeMap::new(),
            utc("2026-08-05T00:00:00Z"),
            utc("2026-09-01T00:00:00Z"),
        );

        assert_eq!(
            starts(&found),
            vec!["2026-08-05 09:00:00"],
            "the 3rd and last occurrence; the first two fell before the window"
        );
    }

    #[test]
    fn a_cancelled_occurrence_is_omitted_and_the_series_continues() {
        let mut exceptions = BTreeMap::new();
        exceptions.insert(local("2026-08-04T09:00:00"), Exception::Cancelled);

        let found = expand(
            &rule("2026-08-03T09:00:00", Some(Freq::Daily)),
            &exceptions,
            utc("2026-08-01T00:00:00Z"),
            utc("2026-08-06T00:00:00Z"),
        );

        assert_eq!(
            starts(&found),
            vec!["2026-08-03 09:00:00", "2026-08-05 09:00:00"]
        );
    }

    /// A cancelled occurrence still spends one of the series' N. The alternative silently extends
    /// a 10-occurrence series every time one is cancelled.
    #[test]
    fn a_cancelled_occurrence_still_spends_its_count() {
        let mut bounded = rule("2026-08-03T09:00:00", Some(Freq::Daily));
        bounded.count = Some(3);
        let mut exceptions = BTreeMap::new();
        exceptions.insert(local("2026-08-04T09:00:00"), Exception::Cancelled);

        let found = expand(
            &bounded,
            &exceptions,
            utc("2026-08-01T00:00:00Z"),
            utc("2026-09-01T00:00:00Z"),
        );

        assert_eq!(
            starts(&found),
            vec!["2026-08-03 09:00:00", "2026-08-05 09:00:00"],
            "three were spent, the middle one cancelled — the series does not gain a fourth"
        );
    }

    /// A moved occurrence keeps reporting its ORIGINAL local start as its identity, which is what
    /// lets it be moved a second time instead of being duplicated.
    #[test]
    fn a_moved_occurrence_keeps_its_original_identity() {
        let mut exceptions = BTreeMap::new();
        exceptions.insert(
            local("2026-08-04T09:00:00"),
            Exception::Moved {
                to_local: local("2026-08-04T15:00:00"),
                duration_minutes: 30,
            },
        );

        let found = expand(
            &rule("2026-08-03T09:00:00", Some(Freq::Daily)),
            &exceptions,
            utc("2026-08-04T00:00:00Z"),
            utc("2026-08-05T00:00:00Z"),
        );

        assert_eq!(found.len(), 1);
        assert_eq!(
            found[0].occurrence_local,
            local("2026-08-04T09:00:00"),
            "identity is where it was, not where it went"
        );
        assert_eq!(
            found[0].start,
            utc("2026-08-04T14:00:00Z"),
            "15:00 in Lisbon in August is 14:00Z"
        );
        assert_eq!(
            found[0].end - found[0].start,
            Duration::minutes(30),
            "a move carries its own duration"
        );
    }

    /// Spring forward: on 29 March 2026 Lisbon jumps 01:00 -> 02:00, so 01:30 does not exist.
    /// Advancing keeps the appointment; skipping would delete it without telling anyone.
    #[test]
    fn an_occurrence_in_a_spring_forward_gap_advances_instead_of_vanishing() {
        let daily = rule("2026-03-28T01:30:00", Some(Freq::Daily));

        let found = expand(
            &daily,
            &BTreeMap::new(),
            utc("2026-03-29T00:00:00Z"),
            utc("2026-03-30T00:00:00Z"),
        );

        assert_eq!(
            starts(&found),
            vec!["2026-03-29 01:30:00"],
            "the occurrence survives the gap"
        );
        assert_eq!(
            found[0].start,
            utc("2026-03-29T01:00:00Z"),
            "the clocks jump 01:00 WET -> 02:00 WEST, so the first wall clock that exists is \
             02:00 — and 02:00 at the NEW +1 offset is 01:00Z"
        );
    }

    /// Autumn back: on 25 October 2026 Lisbon repeats 01:00-02:00, so 01:30 happens twice. The
    /// first is deterministic and is the one lived first.
    #[test]
    fn an_ambiguous_occurrence_resolves_to_the_first_of_the_two() {
        let daily = rule("2026-10-24T01:30:00", Some(Freq::Daily));

        let found = expand(
            &daily,
            &BTreeMap::new(),
            utc("2026-10-25T00:00:00Z"),
            utc("2026-10-26T00:00:00Z"),
        );

        assert_eq!(
            found[0].start,
            utc("2026-10-25T00:30:00Z"),
            "the first 01:30 is still on summer time, one hour ahead of UTC"
        );
    }

    /// The property that makes local storage worth its awkwardness: the wall-clock time does not
    /// drift when the clocks change, even though the instant does.
    #[test]
    fn a_recurring_wall_clock_time_survives_a_daylight_saving_change() {
        let mut weekly = rule("2026-10-19T09:00:00", Some(Freq::Weekly));
        weekly.count = Some(2);

        let found = expand(
            &weekly,
            &BTreeMap::new(),
            utc("2026-10-01T00:00:00Z"),
            utc("2026-11-01T00:00:00Z"),
        );

        assert_eq!(
            starts(&found),
            vec!["2026-10-19 09:00:00", "2026-10-26 09:00:00"],
            "both are 09:00 to the person"
        );
        assert_eq!(found[0].start, utc("2026-10-19T08:00:00Z"), "summer time");
        assert_eq!(
            found[1].start,
            utc("2026-10-26T09:00:00Z"),
            "winter time — same wall clock, a different instant"
        );
    }

    #[test]
    fn an_unparseable_byday_token_is_dropped_without_taking_the_rest_with_it() {
        assert_eq!(
            parse_byday("MO,XX,FR"),
            vec![ByDay::Every(Weekday::Mon), ByDay::Every(Weekday::Fri)]
        );
    }

    #[test]
    fn a_positional_byday_token_parses_its_index() {
        assert_eq!(parse_byday("TH#3"), vec![ByDay::Nth(Weekday::Thu, 3)]);
        assert_eq!(parse_byday("FR#-1"), vec![ByDay::Nth(Weekday::Fri, -1)]);
    }
}
