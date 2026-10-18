//! The one read of finished runs an analytics request makes, and every row derived from it in
//! Rust: each run priced, the token sessions, and (later) the costliest sessions. One query, not
//! four — the blocks are all aggregations of the same rows.

use std::collections::BTreeMap;

use sqlx::SqlitePool;

use super::{TopRun, Window};
use crate::governance::budget::{parse_run_timestamp, time_approx_usd};

/// One finished run, as the analytics reads it.
#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct RunRow {
    pub id: i64,
    pub project_id: Option<String>,
    pub mode: String,
    pub session_id: Option<String>,
    pub created_at: String,
    pub completed_at: String,
    pub cost_usd: Option<f64>,
    pub input_tokens: Option<i64>,
    pub cache_read_tokens: Option<i64>,
    pub cache_creation_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub gate_status: Option<String>,
}

impl RunRow {
    /// The UTC day this run lands on: the day it COMPLETED (spec §4.2).
    pub fn day(&self) -> &str {
        day_of(&self.completed_at)
    }

    /// `session_id`, or the run's own key: a run with no session is its own session, the rule
    /// `session_totals` uses.
    pub fn session_key(&self) -> String {
        self.session_id
            .clone()
            .unwrap_or_else(|| format!("run:{}", self.id))
    }

    /// Measured means the three counts the efficiency baseline needs are all present
    /// (`project/readings.rs` `token_rows`); anything else is unmeasured and never a zero.
    pub fn is_measured(&self) -> bool {
        self.input_tokens.is_some()
            && self.cache_read_tokens.is_some()
            && self.cache_creation_tokens.is_some()
    }
}

/// The UTC day of an RFC3339 timestamp: its first ten characters.
pub fn day_of(timestamp: &str) -> &str {
    timestamp.get(..10).unwrap_or(timestamp)
}

/// Every run that completed in `[from, to)`, for one project or for all.
///
/// `from`/`to` are `YYYY-MM-DD` day bounds: a day string sorts before every timestamp of that day
/// and after every timestamp of the day before, so the comparison is exact whatever offset spelling
/// (`Z` or `+00:00`) the timestamp carries.
pub async fn finished_runs(
    pool: &SqlitePool,
    project: Option<&str>,
    from: &str,
    to: &str,
) -> sqlx::Result<Vec<RunRow>> {
    sqlx::query_as::<_, RunRow>(
        "SELECT id, project_id, mode, session_id, created_at, completed_at, cost_usd,
                input_tokens, cache_read_tokens, cache_creation_tokens, output_tokens, gate_status
           FROM runs
          WHERE completed_at IS NOT NULL AND completed_at >= ?1 AND completed_at < ?2
            AND (?3 IS NULL OR project_id = ?3)
          ORDER BY completed_at, id",
    )
    .bind(from)
    .bind(to)
    .bind(project)
    .fetch_all(pool)
    .await
}

/// One run with what it cost: its reported cost, or — when it reported none — the time
/// approximation the spend ceiling uses, marked so the page can show it apart (spec §4.4).
#[derive(Debug, Clone, PartialEq)]
pub struct Priced<'a> {
    pub row: &'a RunRow,
    pub usd: f64,
    pub estimated: bool,
}

impl Priced<'_> {
    pub fn day(&self) -> &str {
        self.row.day()
    }
}

/// PURE: price every run, run by run (`compute_spend`'s rule). The runs are finished, so the
/// estimate ends at `completed_at`.
pub fn price(rows: &[RunRow], rate_per_hour: f64) -> sqlx::Result<Vec<Priced<'_>>> {
    rows.iter()
        .map(|row| match row.cost_usd {
            Some(usd) => Ok(Priced {
                row,
                usd,
                estimated: false,
            }),
            None => Ok(Priced {
                row,
                usd: time_approx_usd(
                    parse_run_timestamp(&row.created_at)?,
                    parse_run_timestamp(&row.completed_at)?,
                    rate_per_hour,
                ),
                estimated: true,
            }),
        })
        .collect()
}

/// One session's tokens: the largest of each count across its measured runs, attributed to its
/// last run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenSession {
    pub session_key: String,
    pub last_run_id: i64,
    pub project_id: Option<String>,
    pub mode: String,
    /// The last measured run's `completed_at`: the session counts ONCE, on this day.
    pub completed_at: String,
    pub input: i64,
    pub cache_read: i64,
    pub cache_creation: i64,
    /// Not part of "measured" (the three counts above are); a NULL here adds nothing.
    pub output: i64,
}

/// PURE: the four-column sibling of `session_totals` — same criterion (largest kept; no session
/// means a session of its own), each count taking its own maximum, unmeasured runs left out.
pub fn token_sessions(rows: &[RunRow]) -> Vec<TokenSession> {
    let mut sessions: BTreeMap<String, TokenSession> = BTreeMap::new();
    for row in rows.iter().filter(|row| row.is_measured()) {
        let input = row.input_tokens.unwrap_or_default();
        let cache_read = row.cache_read_tokens.unwrap_or_default();
        let cache_creation = row.cache_creation_tokens.unwrap_or_default();
        let output = row.output_tokens.unwrap_or_default();
        let session = sessions
            .entry(row.session_key())
            .or_insert_with(|| TokenSession {
                session_key: row.session_key(),
                last_run_id: row.id,
                project_id: row.project_id.clone(),
                mode: row.mode.clone(),
                completed_at: row.completed_at.clone(),
                input,
                cache_read,
                cache_creation,
                output,
            });
        session.input = session.input.max(input);
        session.cache_read = session.cache_read.max(cache_read);
        session.cache_creation = session.cache_creation.max(cache_creation);
        session.output = session.output.max(output);
        let later = (row.completed_at.as_str(), row.id)
            > (session.completed_at.as_str(), session.last_run_id);
        if later {
            session.last_run_id = row.id;
            session.project_id = row.project_id.clone();
            session.mode = row.mode.clone();
            session.completed_at = row.completed_at.clone();
        }
    }
    sessions.into_values().collect()
}

/// PURE: the costliest sessions among the runs completed in `window` — each session's runs
/// summed, estimated if any was, linked to its last run — cost desc, then the later
/// `completed_at`, then the higher run id; at most `limit`.
pub fn top_sessions(priced: &[Priced<'_>], window: Window, limit: usize) -> Vec<TopRun> {
    let mut sessions: BTreeMap<String, TopRun> = BTreeMap::new();
    for run in priced.iter().filter(|run| window.holds(run.day())) {
        let row = run.row;
        let session = sessions.entry(row.session_key()).or_insert_with(|| TopRun {
            run_id: row.id,
            project_id: row.project_id.clone(),
            mode: row.mode.clone(),
            completed_at: row.completed_at.clone(),
            cost_usd: 0.0,
            cost_is_estimated: false,
        });
        session.cost_usd += run.usd;
        session.cost_is_estimated |= run.estimated;
        let later =
            (row.completed_at.as_str(), row.id) > (session.completed_at.as_str(), session.run_id);
        if later {
            session.run_id = row.id;
            session.project_id = row.project_id.clone();
            session.mode = row.mode.clone();
            session.completed_at = row.completed_at.clone();
        }
    }
    let mut top: Vec<TopRun> = sessions.into_values().collect();
    top.sort_by(|a, b| {
        b.cost_usd
            .total_cmp(&a.cost_usd)
            .then_with(|| b.completed_at.cmp(&a.completed_at))
            .then_with(|| b.run_id.cmp(&a.run_id))
    });
    top.truncate(limit);
    top
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(
        id: i64,
        session: Option<&str>,
        completed: &str,
        tokens: Option<(i64, i64, i64, i64)>,
        cost: Option<f64>,
    ) -> RunRow {
        let (input_tokens, cache_read_tokens, cache_creation_tokens, output_tokens) = match tokens {
            Some((input, read, creation, output)) => {
                (Some(input), Some(read), Some(creation), Some(output))
            }
            None => (None, None, None, None),
        };
        RunRow {
            id,
            project_id: Some("alpha".into()),
            mode: "worktree".into(),
            session_id: session.map(str::to_owned),
            created_at: completed.into(),
            completed_at: completed.into(),
            cost_usd: cost,
            input_tokens,
            cache_read_tokens,
            cache_creation_tokens,
            output_tokens,
            gate_status: None,
        }
    }

    /// Cumulative counts, as the CLI reports them on a resume.
    fn resumed_and_loose() -> Vec<RunRow> {
        vec![
            row(
                1,
                Some("s1"),
                "2026-10-01T23:50:00Z",
                Some((100, 10, 5, 20)),
                None,
            ),
            row(
                2,
                Some("s1"),
                "2026-10-02T00:10:00Z",
                Some((300, 30, 15, 40)),
                None,
            ),
            row(3, None, "2026-10-02T09:00:00Z", Some((50, 0, 0, 5)), None),
            row(4, None, "2026-10-02T10:00:00Z", Some((50, 0, 0, 5)), None),
        ]
    }

    #[test]
    fn a_session_counts_the_largest_of_each_count_once_on_its_last_run() {
        let mut rows = resumed_and_loose();
        rows.push(row(5, Some("s2"), "2026-10-02T11:00:00Z", None, None));

        let sessions = token_sessions(&rows);

        assert_eq!(
            sessions.len(),
            3,
            "s1 once, two loose runs; unmeasured s2 is no session"
        );
        let s1 = sessions.iter().find(|s| s.session_key == "s1").unwrap();
        assert_eq!(
            (s1.input, s1.cache_read, s1.cache_creation, s1.output),
            (300, 30, 15, 40)
        );
        assert_eq!(s1.last_run_id, 2);
        // Crossed midnight: counted once, on the day of its last run.
        assert_eq!(day_of(&s1.completed_at), "2026-10-02");
        assert!(sessions.iter().any(|s| s.session_key == "run:3"));
        assert!(sessions.iter().any(|s| s.session_key == "run:4"));
    }

    /// The four-column grouping is a sibling of `session_totals`, not a second opinion: on the
    /// cumulative counts both assume, the per-session total is the same number.
    #[test]
    fn the_token_total_per_session_agrees_with_session_totals() {
        let rows = resumed_and_loose();
        let ours: i64 = token_sessions(&rows)
            .iter()
            .map(|s| s.input + s.cache_read + s.cache_creation)
            .sum();
        let pairs: Vec<(Option<String>, i64)> = rows
            .iter()
            .map(|r| {
                (
                    r.session_id.clone(),
                    r.input_tokens.unwrap()
                        + r.cache_read_tokens.unwrap()
                        + r.cache_creation_tokens.unwrap(),
                )
            })
            .collect();
        let theirs: i64 = crate::project::readings::session_totals(&pairs)
            .iter()
            .sum();
        assert_eq!(ours, theirs);
        assert_eq!(ours, 445);
    }

    #[test]
    fn a_run_without_a_reported_cost_is_priced_from_its_duration_and_marked() {
        let mut unknown = row(1, None, "2026-10-02T10:00:00Z", None, None);
        unknown.created_at = "2026-10-02T09:00:00Z".into();
        let rows = vec![
            unknown,
            row(2, None, "2026-10-02T10:00:00Z", None, Some(1.25)),
        ];

        let priced = price(&rows, 3.0).unwrap();

        assert!(priced[0].estimated);
        assert!((priced[0].usd - 3.0).abs() < 1e-9, "one hour at $3/h");
        assert!(!priced[1].estimated);
        assert!((priced[1].usd - 1.25).abs() < 1e-9);
    }

    /// The card that showed run 900677 at "$133.71 est.": a browser-panel turn with no reported
    /// cost, open 44.6 hours. The page prices with the ceiling's own approximation, so it carries
    /// the same four-hour cap — $12 at $3/h, still marked as an estimate.
    #[test]
    fn a_forgotten_run_is_priced_at_the_capped_estimate() {
        let mut forgotten = row(900677, None, "2026-10-17T16:44:50Z", None, None);
        forgotten.created_at = "2026-10-15T20:10:41Z".into();
        forgotten.mode = "assistant".into();
        forgotten.project_id = None;
        let rows = vec![forgotten];

        let priced = price(&rows, 3.0).unwrap();

        assert!(priced[0].estimated);
        assert!(
            (priced[0].usd - 12.0).abs() < 1e-9,
            "four hours at $3/h, got {}",
            priced[0].usd
        );
    }
}
