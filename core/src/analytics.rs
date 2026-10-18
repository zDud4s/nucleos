//! Trends over the daemon's own history — cost, tokens, gate outcomes, merges and quota — as daily
//! series over a 7/30/90-day UTC window, each with the window before it for comparison (spec
//! 2026-10-17-analytics-fundacao).
//!
//! Computed on request, no rollup: one SELECT of finished runs (`sessions.rs`), one of judge calls,
//! one of merges and one count of runs in flight per request; every block is derived from those by
//! the pure functions below, so a test states its rows and its clock and reads the answer.
//!
//! **Two dedupe rules, side by side on purpose.** Cost is summed run by run — `compute_spend`'s
//! argument (`budget.rs`): `--resume` reports its own invocation, not a running total. Tokens take
//! the largest value per session — `session_totals`' rule (`project/readings.rs`): the token counts
//! a resumed run reports ARE cumulative. Different bases, kept different.

pub mod cost;
pub mod delivered;
pub mod devtime;
pub mod quota;
pub mod sessions;

use std::collections::BTreeMap;

use chrono::{DateTime, Duration, NaiveDate, SecondsFormat, Utc};
use serde::Serialize;
use sqlx::SqlitePool;

pub use self::cost::{
    CostBlock, CostKey, CostPart, CostPoint, CostTotal, JUDGE_KEY, NO_PROJECT_KEY, OTHER_KEY,
};
pub use self::delivered::{DeliveredBlock, DeliveredPoint, DeliveredTotal};
pub use self::quota::{QuotaAnalytics, QuotaPoint, QuotaSeries, quota_history};
use self::sessions::{RunRow, TokenSession, day_of};

/// The windows served. Anything else is the client's error (400), never clamped (spec §4.1).
pub const ALLOWED_DAYS: [i64; 3] = [7, 30, 90];
pub const DEFAULT_DAYS: i64 = 30;
const TOP_RUNS: usize = 10;

/// Whose history a report reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope<'a> {
    Global,
    Project(&'a str),
}

impl<'a> Scope<'a> {
    fn project(self) -> Option<&'a str> {
        match self {
            Scope::Global => None,
            Scope::Project(id) => Some(id),
        }
    }
}

/// Whole UTC days `[from, to_exclusive)`, ending with the day `now` falls on (spec §4.1).
///
/// Aligned to days so there is no half bucket at the start; the last day is today and is partial.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Window {
    pub days: i64,
    pub from: NaiveDate,
    pub to_exclusive: NaiveDate,
}

impl Window {
    /// `None` for any `days` outside [`ALLOWED_DAYS`].
    pub fn new(days: i64, now: DateTime<Utc>) -> Option<Self> {
        if !ALLOWED_DAYS.contains(&days) {
            return None;
        }
        let to_exclusive = now.date_naive() + Duration::days(1);
        Some(Self {
            days,
            from: to_exclusive - Duration::days(days),
            to_exclusive,
        })
    }

    /// The window of equal length immediately before this one.
    pub fn previous(self) -> Self {
        Self {
            days: self.days,
            from: self.from - Duration::days(self.days),
            to_exclusive: self.from,
        }
    }

    /// Every day of the window, `YYYY-MM-DD`, oldest first — exactly `days` of them.
    pub fn day_list(self) -> Vec<String> {
        (0..self.days)
            .map(|offset| (self.from + Duration::days(offset)).to_string())
            .collect()
    }

    /// The SQL lower bound, `YYYY-MM-DD` (see `sessions::finished_runs` for why a day string).
    pub fn from_bound(self) -> String {
        self.from.to_string()
    }

    /// The SQL upper bound, exclusive.
    pub fn to_bound(self) -> String {
        self.to_exclusive.to_string()
    }

    /// Whether a `YYYY-MM-DD` bucket falls in this window.
    pub fn holds(self, day: &str) -> bool {
        NaiveDate::parse_from_str(day, "%Y-%m-%d")
            .is_ok_and(|day| day >= self.from && day < self.to_exclusive)
    }

    /// First instant of the window.
    pub fn start(self) -> DateTime<Utc> {
        midnight(self.from)
    }

    /// First instant after the window.
    pub fn end(self) -> DateTime<Utc> {
        midnight(self.to_exclusive)
    }

    /// Inclusive day labels, as the page prints them.
    pub fn span(self) -> Span {
        Span {
            from: self.from.to_string(),
            to: (self.to_exclusive - Duration::days(1)).to_string(),
        }
    }
}

fn midnight(day: NaiveDate) -> DateTime<Utc> {
    day.and_hms_opt(0, 0, 0)
        .expect("midnight is valid")
        .and_utc()
}

/* ------------------------------------------------------------- response -- */

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Span {
    pub from: String,
    pub to: String,
}

/// Fixed: every mode is counted (spec decision 2). The page reads it for its coverage note.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Counted {
    pub modes: &'static str,
}

/// `GET /analytics` and `GET /projects/{id}/analytics`, one shape for both (spec §6).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Analytics {
    pub scope: &'static str,
    pub project_id: Option<String>,
    pub days: i64,
    pub timezone: &'static str,
    pub now: String,
    pub window: Span,
    pub previous: Span,
    pub counted: Counted,
    pub cost: CostBlock,
    pub tokens: TokensBlock,
    pub throughput: ThroughputBlock,
    pub delivered: DeliveredBlock,
    /// The ten costliest sessions of the window (spec §4.8). Runs only: the judge has no run.
    pub top_runs: Vec<TopRun>,
}

/// Gate outcomes of finished runs. Five counters so they always add up to `runs_completed`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
pub struct GateTotal {
    pub runs_completed: i64,
    pub passed: i64,
    pub failed: i64,
    pub errored: i64,
    pub no_gate: i64,
    /// Any literal the núcleo does not write as a verdict (`running`, a future state). Counted
    /// here rather than dropped, unlike `tally_gates`, so the sum still matches.
    pub other: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GatePoint {
    pub day: String,
    pub partial: bool,
    pub passed: i64,
    pub failed: i64,
    pub errored: i64,
    pub no_gate: i64,
    pub other: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ModeCount {
    pub mode: String,
    pub runs_completed: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ThroughputBlock {
    pub total: GateTotal,
    pub previous: GateTotal,
    pub series: Vec<GatePoint>,
    /// Current window only; runs desc, then mode.
    pub by_mode: Vec<ModeCount>,
    /// Runs with no `completed_at`, whenever they started: informative, in no window.
    pub in_flight: i64,
}

/// Token totals over measured sessions. Absent is `null`, never zero (spec §4.4).
#[derive(Debug, Clone, PartialEq, Default, Serialize)]
pub struct TokenTotal {
    pub input: i64,
    pub cache_read: i64,
    pub cache_creation: i64,
    pub output: i64,
    pub measured_sessions: i64,
    /// Runs in the window missing any of the three measured counts.
    pub unmeasured_runs: i64,
    /// `cache_read / (input + cache_read + cache_creation)`; `None` with no denominator.
    pub cache_hit_ratio: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
pub struct TokenPoint {
    pub day: String,
    pub partial: bool,
    pub input: i64,
    pub cache_read: i64,
    pub cache_creation: i64,
    pub output: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TokensBlock {
    pub total: TokenTotal,
    pub previous: TokenTotal,
    pub series: Vec<TokenPoint>,
}

/// One of the costliest sessions, linked to its last run (`/runs/$id`).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TopRun {
    pub run_id: i64,
    pub project_id: Option<String>,
    pub mode: String,
    pub completed_at: String,
    /// The session's runs in the window, summed (measured and estimated alike).
    pub cost_usd: f64,
    /// True when any of those runs was estimated.
    pub cost_is_estimated: bool,
}

/* --------------------------------------------------------------- report -- */

/// Everything the analytics page shows for one scope and window.
///
/// `now` is a parameter so a test can state its own clock.
pub async fn report(
    pool: &SqlitePool,
    scope: Scope<'_>,
    window: Window,
    now: DateTime<Utc>,
) -> sqlx::Result<Analytics> {
    let previous = window.previous();
    let project = scope.project();
    let (from, to) = (previous.from_bound(), window.to_bound());

    let rows = sessions::finished_runs(pool, project, &from, &to).await?;
    let in_flight: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM runs
          WHERE completed_at IS NULL AND (?1 IS NULL OR project_id = ?1)",
    )
    .bind(project)
    .fetch_one(pool)
    .await?;

    let rate = crate::governance::budget::load_budget_config(pool)
        .await?
        .time_cost_per_hour_usd;
    let priced = sessions::price(&rows, rate)?;
    let judge = cost::judge_lines(pool, project, &from, &to).await?;
    let merges = delivered::merge_lines(pool, project, &from, &to).await?;

    Ok(Analytics {
        scope: if project.is_some() {
            "project"
        } else {
            "global"
        },
        project_id: project.map(str::to_owned),
        days: window.days,
        timezone: "UTC",
        now: now.to_rfc3339_opts(SecondsFormat::Secs, true),
        window: window.span(),
        previous: previous.span(),
        counted: Counted { modes: "all" },
        cost: cost::cost_block(scope, window, &priced, &judge),
        tokens: tokens_block(window, &rows),
        throughput: throughput_block(window, &rows, in_flight),
        delivered: delivered::delivered_block(window, &merges),
        top_runs: sessions::top_sessions(&priced, window, TOP_RUNS),
    })
}

/* ------------------------------------------------------------ throughput -- */

/// PURE: the five gate counters over a set of finished runs (spec §4.6).
pub fn tally<'a>(statuses: impl IntoIterator<Item = Option<&'a str>>) -> GateTotal {
    let mut total = GateTotal::default();
    for status in statuses {
        total.runs_completed += 1;
        match status {
            None => total.no_gate += 1,
            Some("passed") => total.passed += 1,
            Some("failed") => total.failed += 1,
            Some("errored") => total.errored += 1,
            Some(_) => total.other += 1,
        }
    }
    total
}

/// The runs of `rows` that completed inside `window`.
fn in_window(window: Window, rows: &[RunRow]) -> impl Iterator<Item = &RunRow> {
    rows.iter().filter(move |row| window.holds(row.day()))
}

fn throughput_block(window: Window, rows: &[RunRow], in_flight: i64) -> ThroughputBlock {
    let days = window.day_list();
    let last = days.len() - 1;
    // Bucketed once by day, then the day list is mapped through it: O(rows + days).
    let mut by_day: BTreeMap<&str, Vec<&RunRow>> = BTreeMap::new();
    for row in rows {
        by_day.entry(row.day()).or_default().push(row);
    }
    let series = days
        .iter()
        .enumerate()
        .map(|(index, day)| {
            let t = tally(
                by_day
                    .get(day.as_str())
                    .into_iter()
                    .flatten()
                    .map(|row| row.gate_status.as_deref()),
            );
            GatePoint {
                day: day.clone(),
                partial: index == last,
                passed: t.passed,
                failed: t.failed,
                errored: t.errored,
                no_gate: t.no_gate,
                other: t.other,
            }
        })
        .collect();

    let mut modes: BTreeMap<&str, i64> = BTreeMap::new();
    for row in in_window(window, rows) {
        *modes.entry(row.mode.as_str()).or_default() += 1;
    }
    let mut by_mode: Vec<ModeCount> = modes
        .into_iter()
        .map(|(mode, runs_completed)| ModeCount {
            mode: mode.to_owned(),
            runs_completed,
        })
        .collect();
    by_mode.sort_by(|a, b| {
        b.runs_completed
            .cmp(&a.runs_completed)
            .then_with(|| a.mode.cmp(&b.mode))
    });

    ThroughputBlock {
        total: tally(in_window(window, rows).map(|row| row.gate_status.as_deref())),
        previous: tally(in_window(window.previous(), rows).map(|row| row.gate_status.as_deref())),
        series,
        by_mode,
        in_flight,
    }
}

/* ---------------------------------------------------------------- tokens -- */

fn tokens_total(window: Window, sessions: &[TokenSession], rows: &[RunRow]) -> TokenTotal {
    let mut total = TokenTotal::default();
    for session in sessions
        .iter()
        .filter(|session| window.holds(day_of(&session.completed_at)))
    {
        total.input += session.input;
        total.cache_read += session.cache_read;
        total.cache_creation += session.cache_creation;
        total.output += session.output;
        total.measured_sessions += 1;
    }
    total.unmeasured_runs = rows
        .iter()
        .filter(|row| window.holds(row.day()) && !row.is_measured())
        .count() as i64;
    let read = total.input + total.cache_read + total.cache_creation;
    total.cache_hit_ratio = if total.measured_sessions > 0 && read > 0 {
        Some(total.cache_read as f64 / read as f64)
    } else {
        None
    };
    total
}

/// Sessions are grouped over BOTH windows' rows and land in the window of their last run, so a
/// session that straddles the boundary counts once, in the later window (spec §13.4).
fn tokens_block(window: Window, rows: &[RunRow]) -> TokensBlock {
    let sessions = sessions::token_sessions(rows);
    let days = window.day_list();
    let last = days.len() - 1;
    let mut by_day: BTreeMap<&str, Vec<&TokenSession>> = BTreeMap::new();
    for session in &sessions {
        by_day
            .entry(day_of(&session.completed_at))
            .or_default()
            .push(session);
    }
    let series = days
        .iter()
        .enumerate()
        .map(|(index, day)| {
            let mut point = TokenPoint {
                day: day.clone(),
                partial: index == last,
                ..TokenPoint::default()
            };
            for session in by_day.get(day.as_str()).into_iter().flatten() {
                point.input += session.input;
                point.cache_read += session.cache_read;
                point.cache_creation += session.cache_creation;
                point.output += session.output;
            }
            point
        })
        .collect();
    TokensBlock {
        total: tokens_total(window, &sessions, rows),
        previous: tokens_total(window.previous(), &sessions, rows),
        series,
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn at(moment: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(moment)
            .unwrap()
            .with_timezone(&Utc)
    }

    /// The spec's own example clock (§6).
    pub(crate) fn now() -> DateTime<Utc> {
        at("2026-10-17T14:03:11Z")
    }

    /// One run as a test describes it. `done` is a measured, finished, gate-less worktree run.
    #[derive(Clone)]
    pub(crate) struct Run {
        pub(crate) project: Option<&'static str>,
        pub(crate) mode: &'static str,
        pub(crate) session: Option<&'static str>,
        pub(crate) created: &'static str,
        pub(crate) completed: Option<&'static str>,
        pub(crate) tokens: Option<(i64, i64, i64, i64)>,
        pub(crate) gate: Option<&'static str>,
        pub(crate) cost: Option<f64>,
    }

    pub(crate) fn done(project: &'static str, completed: &'static str) -> Run {
        Run {
            project: Some(project),
            mode: "worktree",
            session: None,
            created: completed,
            completed: Some(completed),
            tokens: Some((100, 0, 0, 10)),
            gate: None,
            cost: Some(1.0),
        }
    }

    pub(crate) async fn insert(pool: &SqlitePool, run: Run) -> i64 {
        let (input, cache_read, cache_creation, output) = match run.tokens {
            Some((a, b, c, d)) => (Some(a), Some(b), Some(c), Some(d)),
            None => (None, None, None, None),
        };
        sqlx::query_scalar(
            "INSERT INTO runs (prompt, status, mode, project_id, session_id, created_at,
                               completed_at, input_tokens, cache_read_tokens,
                               cache_creation_tokens, output_tokens, gate_status, cost_usd)
             VALUES ('p', 'completed', ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) RETURNING id",
        )
        .bind(run.mode)
        .bind(run.project)
        .bind(run.session)
        .bind(run.created)
        .bind(run.completed)
        .bind(input)
        .bind(cache_read)
        .bind(cache_creation)
        .bind(output)
        .bind(run.gate)
        .bind(run.cost)
        .fetch_one(pool)
        .await
        .unwrap()
    }

    pub(crate) async fn report_of(pool: &SqlitePool, scope: Scope<'_>, days: i64) -> Analytics {
        report(pool, scope, Window::new(days, now()).unwrap(), now())
            .await
            .unwrap()
    }

    /* ------------------------------------------------------------- window -- */

    /// Spec test 1: exactly `days` points, zeros where nothing happened, only today partial.
    #[tokio::test]
    async fn every_window_has_exactly_its_days_and_only_today_is_partial() {
        let pool = crate::testdb::fresh_pool().await;
        for days in ALLOWED_DAYS {
            let report = report_of(&pool, Scope::Global, days).await;
            let series = &report.throughput.series;
            assert_eq!(series.len() as i64, days);
            assert_eq!(series.last().unwrap().day, "2026-10-17");
            let first = NaiveDate::from_ymd_opt(2026, 10, 18).unwrap() - Duration::days(days);
            assert_eq!(series[0].day, first.to_string());
            assert!(series.last().unwrap().partial);
            assert!(series[..series.len() - 1].iter().all(|p| !p.partial));
            assert!(
                series
                    .iter()
                    .all(|p| p.passed + p.failed + p.errored + p.no_gate + p.other == 0)
            );
        }

        assert_eq!(
            Window::new(14, now()),
            None,
            "no clamp: 14 is the client's error"
        );
        let window = Window::new(30, now()).unwrap();
        assert_eq!(
            window.span(),
            Span {
                from: "2026-09-18".into(),
                to: "2026-10-17".into()
            }
        );
        assert_eq!(
            window.previous().span(),
            Span {
                from: "2026-08-19".into(),
                to: "2026-09-17".into()
            }
        );
        let report = report_of(&pool, Scope::Global, 30).await;
        assert_eq!(report.scope, "global");
        assert_eq!(report.project_id, None);
        assert_eq!(report.timezone, "UTC");
        assert_eq!(report.now, "2026-10-17T14:03:11Z");
        assert_eq!(report.counted.modes, "all");
    }

    /// Spec test 2: the window before is as long, and both are `[from, to)`.
    #[tokio::test]
    async fn the_previous_window_is_as_long_and_each_window_is_closed_on_the_left() {
        let pool = crate::testdb::fresh_pool().await;
        for completed in [
            "2026-09-18T00:00:00Z", // current, first instant
            "2026-09-17T23:59:59Z", // previous, last instant
            "2026-08-19T00:00:00Z", // previous, first instant
            "2026-08-18T23:59:59Z", // neither
            "2026-10-18T00:00:00Z", // neither: tomorrow
        ] {
            insert(&pool, done("alpha", completed)).await;
        }

        let report = report_of(&pool, Scope::Global, 30).await;

        assert_eq!(report.throughput.total.runs_completed, 1);
        assert_eq!(report.throughput.previous.runs_completed, 2);
    }

    /* --------------------------------------------------------- throughput -- */

    /// Spec test 5: every literal lands somewhere and the five always add up to the run count.
    #[tokio::test]
    async fn every_gate_literal_is_counted_and_the_sum_is_the_run_count() {
        let pool = crate::testdb::fresh_pool().await;
        for gate in [
            Some("passed"),
            Some("failed"),
            Some("errored"),
            None,
            Some("running"),
        ] {
            insert(
                &pool,
                Run {
                    gate,
                    ..done("alpha", "2026-10-10T12:00:00Z")
                },
            )
            .await;
        }
        // In flight: no `completed_at`, so outside every window and counted apart.
        insert(
            &pool,
            Run {
                completed: None,
                ..done("alpha", "2026-10-17T10:00:00Z")
            },
        )
        .await;

        let report = report_of(&pool, Scope::Project("alpha"), 30).await;

        assert_eq!(
            report.throughput.total,
            GateTotal {
                runs_completed: 5,
                passed: 1,
                failed: 1,
                errored: 1,
                no_gate: 1,
                other: 1
            }
        );
        let day = report
            .throughput
            .series
            .iter()
            .find(|p| p.day == "2026-10-10")
            .unwrap();
        assert_eq!(
            (day.passed, day.failed, day.errored, day.no_gate, day.other),
            (1, 1, 1, 1, 1)
        );
        assert_eq!(report.throughput.in_flight, 1);
        assert_eq!(report.scope, "project");
        assert_eq!(report.project_id.as_deref(), Some("alpha"));
    }

    /// Spec test 7: every mode is counted under its raw name, an unknown one too.
    #[tokio::test]
    async fn the_mode_breakdown_keeps_every_mode_even_an_unknown_one() {
        let pool = crate::testdb::fresh_pool().await;
        for mode in ["worktree", "worktree", "assistant", "weird_new_mode"] {
            insert(
                &pool,
                Run {
                    mode,
                    ..done("alpha", "2026-10-10T12:00:00Z")
                },
            )
            .await;
        }

        let report = report_of(&pool, Scope::Global, 30).await;

        assert_eq!(
            report.throughput.by_mode,
            vec![
                ModeCount {
                    mode: "worktree".into(),
                    runs_completed: 2
                },
                ModeCount {
                    mode: "assistant".into(),
                    runs_completed: 1
                },
                ModeCount {
                    mode: "weird_new_mode".into(),
                    runs_completed: 1
                },
            ]
        );
    }

    /// Spec test 10 (isolation half): a project's report holds that project's runs only.
    #[tokio::test]
    async fn another_project_is_not_in_this_project_s_report() {
        let pool = crate::testdb::fresh_pool().await;
        insert(&pool, done("alpha", "2026-10-10T12:00:00Z")).await;
        insert(&pool, done("beta", "2026-10-10T12:00:00Z")).await;

        assert_eq!(
            report_of(&pool, Scope::Project("alpha"), 30)
                .await
                .throughput
                .total
                .runs_completed,
            1
        );
        assert_eq!(
            report_of(&pool, Scope::Global, 30)
                .await
                .throughput
                .total
                .runs_completed,
            2
        );
    }

    pub(crate) fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    /* ------------------------------------------------------------- tokens -- */

    fn tokens_on<'r>(report: &'r Analytics, day: &str) -> &'r TokenPoint {
        report
            .tokens
            .series
            .iter()
            .find(|point| point.day == day)
            .unwrap_or_else(|| panic!("no token point for {day}"))
    }

    /// Spec test 3 (tokens): a session that crosses midnight counts its MAX once, on the day of
    /// its last run; a run with no session is a session of its own.
    #[tokio::test]
    async fn a_session_counts_its_tokens_once_on_the_day_of_its_last_run() {
        let pool = crate::testdb::fresh_pool().await;
        insert(
            &pool,
            Run {
                session: Some("s1"),
                tokens: Some((100, 10, 5, 20)),
                ..done("alpha", "2026-10-01T23:50:00Z")
            },
        )
        .await;
        insert(
            &pool,
            Run {
                session: Some("s1"),
                tokens: Some((300, 30, 15, 40)),
                ..done("alpha", "2026-10-02T00:10:00Z")
            },
        )
        .await;
        for completed in ["2026-10-02T09:00:00Z", "2026-10-02T10:00:00Z"] {
            insert(
                &pool,
                Run {
                    tokens: Some((50, 0, 0, 5)),
                    ..done("alpha", completed)
                },
            )
            .await;
        }

        let report = report_of(&pool, Scope::Project("alpha"), 30).await;

        assert_eq!(report.tokens.series.len(), 30);
        assert_eq!(tokens_on(&report, "2026-10-01").input, 0, "no double count");
        let day = tokens_on(&report, "2026-10-02");
        assert_eq!(
            (day.input, day.cache_read, day.cache_creation, day.output),
            (400, 30, 15, 50)
        );
        assert_eq!(report.tokens.total.measured_sessions, 3);
        assert_eq!(report.tokens.total.input, 400);
    }

    /// A session whose last run is the first day of the current window counts there, never in the
    /// previous window, even though its earlier run sits on the previous window's last day.
    #[tokio::test]
    async fn a_session_straddling_the_window_boundary_counts_once_in_the_current_window() {
        let pool = crate::testdb::fresh_pool().await;
        // now() is 2026-10-17, so the 7-day window opens on 10-11 and the previous one ends 10-10.
        for completed in ["2026-10-10T23:50:00Z", "2026-10-11T00:10:00Z"] {
            insert(
                &pool,
                Run {
                    session: Some("s1"),
                    ..done("alpha", completed)
                },
            )
            .await;
        }

        let report = report_of(&pool, Scope::Project("alpha"), 7).await;

        assert_eq!(report.tokens.previous.measured_sessions, 0);
        assert_eq!(report.tokens.total.measured_sessions, 1);
    }

    /// Spec test 4 (tokens): silence is counted apart, never as a zero; no denominator, no ratio.
    #[tokio::test]
    async fn an_unmeasured_run_is_counted_apart_and_never_as_a_zero() {
        let pool = crate::testdb::fresh_pool().await;
        insert(
            &pool,
            Run {
                tokens: None,
                ..done("alpha", "2026-10-02T10:00:00Z")
            },
        )
        .await;
        insert(
            &pool,
            Run {
                tokens: Some((60, 30, 10, 5)),
                ..done("alpha", "2026-10-02T11:00:00Z")
            },
        )
        .await;

        let report = report_of(&pool, Scope::Global, 30).await;

        assert_eq!(report.tokens.total.unmeasured_runs, 1);
        assert_eq!(report.tokens.total.measured_sessions, 1);
        assert_eq!(report.tokens.total.input, 60);
        assert!(close(report.tokens.total.cache_hit_ratio.unwrap(), 0.3));
        assert_eq!(
            report.tokens.previous.cache_hit_ratio, None,
            "no sessions, no ratio"
        );

        let empty = crate::testdb::fresh_pool().await;
        insert(
            &empty,
            Run {
                tokens: Some((0, 0, 0, 0)),
                ..done("alpha", "2026-10-02T11:00:00Z")
            },
        )
        .await;
        let report = report_of(&empty, Scope::Global, 30).await;
        assert_eq!(report.tokens.total.measured_sessions, 1);
        assert_eq!(
            report.tokens.total.cache_hit_ratio, None,
            "a zero denominator is no ratio"
        );
    }

    /* ----------------------------------------------------------- top runs -- */

    /// Spec test 8: ten at most, by session cost, ties to the later run then the higher id, each
    /// linking the session's LAST run; a session with an estimated run is marked.
    #[tokio::test]
    async fn the_costliest_sessions_come_ten_at_most_in_order() {
        let pool = crate::testdb::fresh_pool().await;
        const AT: [&str; 11] = [
            "2026-10-10T01:00:00Z",
            "2026-10-10T02:00:00Z",
            "2026-10-10T03:00:00Z",
            "2026-10-10T04:00:00Z",
            "2026-10-10T05:00:00Z",
            "2026-10-10T06:00:00Z",
            "2026-10-10T07:00:00Z",
            "2026-10-10T08:00:00Z",
            "2026-10-10T09:00:00Z",
            "2026-10-10T10:00:00Z",
            "2026-10-10T11:00:00Z",
        ];
        const SESSIONS: [&str; 11] = [
            "s1", "s2", "s3", "s4", "s5", "s6", "s7", "s8", "s9", "s10", "s11",
        ];
        let mut ids = Vec::new();
        for (index, (session, at)) in SESSIONS.iter().zip(AT).enumerate() {
            let id = insert(
                &pool,
                Run {
                    session: Some(*session),
                    cost: Some((index + 1) as f64),
                    ..done("alpha", at)
                },
            )
            .await;
            ids.push(id);
        }
        // Same cost as s6, finished later: it goes first.
        let s13 = insert(
            &pool,
            Run {
                session: Some("s13"),
                cost: Some(6.0),
                ..done("alpha", "2026-10-10T20:00:00Z")
            },
        )
        .await;
        // Two runs, one estimated (30 min at $3/h = 1.5): 6.0 + 1.5 = 7.5, linked to the second.
        insert(
            &pool,
            Run {
                session: Some("s12"),
                cost: Some(6.0),
                ..done("alpha", "2026-10-11T01:00:00Z")
            },
        )
        .await;
        let s12_last = insert(
            &pool,
            Run {
                session: Some("s12"),
                created: "2026-10-11T02:00:00Z",
                cost: None,
                ..done("alpha", "2026-10-11T02:30:00Z")
            },
        )
        .await;

        let report = report_of(&pool, Scope::Global, 30).await;
        let got: Vec<(i64, f64, bool)> = report
            .top_runs
            .iter()
            .map(|top| (top.run_id, top.cost_usd, top.cost_is_estimated))
            .collect();

        assert_eq!(
            got,
            vec![
                (ids[10], 11.0, false),
                (ids[9], 10.0, false),
                (ids[8], 9.0, false),
                (ids[7], 8.0, false),
                (s12_last, 7.5, true),
                (ids[6], 7.0, false),
                (s13, 6.0, false),
                (ids[5], 6.0, false),
                (ids[4], 5.0, false),
                (ids[3], 4.0, false),
            ]
        );
        assert_eq!(report.top_runs[4].completed_at, "2026-10-11T02:30:00Z");
        assert_eq!(report.top_runs[4].project_id.as_deref(), Some("alpha"));
        assert_eq!(report.top_runs[4].mode, "worktree");
    }

    /// Two sessions with equal cost and the same completion instant: the higher run id first.
    #[tokio::test]
    async fn equal_cost_and_completion_put_the_higher_run_id_first() {
        let pool = crate::testdb::fresh_pool().await;
        let mut ids = Vec::new();
        for session in ["tie-a", "tie-b"] {
            let run = Run {
                session: Some(session),
                cost: Some(4.0),
                ..done("alpha", "2026-10-10T05:00:00Z")
            };
            ids.push(insert(&pool, run).await);
        }

        let report = report_of(&pool, Scope::Global, 30).await;
        let got: Vec<i64> = report.top_runs.iter().map(|top| top.run_id).collect();

        assert_eq!(got, vec![ids[1], ids[0]]);
    }
}
