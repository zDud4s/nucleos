//! The four readings a project's workspace leads with.
//!
//! One route rather than four, because all four are aggregations over the *same* rows in the same
//! window: a project's finished runs. Three round trips would be three walks of one table for one
//! panel.
//!
//! Two things here are not the obvious arithmetic, and both were read out of the modules that
//! already own these numbers rather than invented:
//!
//! **A session is counted once.** A resumed run reports the CUMULATIVE session total — both
//! `budget.rs` and `token_efficiency.rs` say so and both dedupe for it — so a plain `SUM` over rows
//! counts the same tokens and the same dollars several times over. A three-node job that resumed
//! twice would read as if it had cost triple.
//!
//! **A gate that could not run is not a gate that failed, and neither is a project with no gate.**
//! `0032_run_gate.sql` states the third case outright: NULL is the honest value when a project has
//! no gate command, because that project has no definition of green for the daemon to measure.
//! Folding the three into one "unmeasured" number would be the exact collapse the design forbids —
//! one of them says the code is broken, one says the measurement broke, and one says nobody ever
//! asked for a measurement.

use chrono::{DateTime, Duration, Utc};
use sqlx::SqlitePool;

use crate::token_efficiency::Baseline;

/// How far back a reading looks when the caller does not say.
pub const DEFAULT_WINDOW_DAYS: i64 = 30;

/// The widest window that will be served.
///
/// Not a performance guard — the queries are bounded by `completed_at` and ride the same index the
/// efficiency baseline does. It is here because a window measured in years is not a reading of *how
/// this project is going*, it is a report, and answering one from the panel that is supposed to say
/// "now" would put a slow query behind a three-second poll.
pub const MAX_WINDOW_DAYS: i64 = 365;

/// How hard this project is working for its tokens.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Efficiency {
    /// Sessions in the window that reported all three token counts.
    pub measured_runs: i64,
    /// Runs that reported nothing.
    ///
    /// Its own field and never folded into the median's denominator. A Codex or local-model daemon
    /// reports no usage at all — `token_efficiency.rs` calls that "the common one" — and counting
    /// those silences as zero would drag the median down until every cloud run looked like drift.
    pub unmeasured_runs: i64,
    /// The middle session's total, or `None` when nothing in the window could be measured.
    pub median_total_tokens: Option<i64>,
    /// The same statistic over the window immediately before this one, so the page can show
    /// movement rather than a number with nothing to be compared against.
    pub previous_median_total_tokens: Option<i64>,
}

/// What this project spent.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Cost {
    pub usd: f64,
    /// Runs the figure was computed over, before session dedupe. Shown so that a large number
    /// standing on three runs cannot read as a trend.
    pub runs: i64,
}

/// How the daemon's own verification went. Four fields, and they are four on purpose.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize)]
pub struct Gate {
    pub passed: i64,
    /// The gate ran and said no. This is the one that means the code is broken.
    pub failed: i64,
    /// The gate could not run — the command was missing, the worktree was gone. Says nothing at all
    /// about the code.
    pub errored: i64,
    /// This project has no gate command, so there was nothing to run. Not a failure and not an
    /// error: nobody asked for a measurement.
    pub no_gate: i64,
}

/// What actually reached the integration branch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize)]
pub struct Delivered {
    /// Merges the queue completed for this project in the window.
    pub landed: i64,
    /// How many of those could be timed — a merge somebody asked for by hand has no run behind it
    /// and therefore no start. Reported so that a median over four of twenty is visible as such.
    pub timed: i64,
    /// Median minutes from a run starting to its work being merged.
    pub median_minutes: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Readings {
    pub window_days: i64,
    pub efficiency: Efficiency,
    pub cost: Cost,
    pub gate: Gate,
    pub delivered: Delivered,
}

/* ------------------------------------------------------------------- pure -- */

/// PURE: one total per session, largest kept.
///
/// The largest and not the most recent, which is the rule `budget.rs` argues for at length: that a
/// resumed run reports a cumulative total is an assumption about the CLI's output, not something
/// this can verify, and a restart or a version change can report less than the session already
/// used. Taking the maximum rounds the same way the spend ceiling does.
///
/// A row with no session is its own session — it cannot be a resumption of anything.
pub fn session_totals(rows: &[(Option<String>, i64)]) -> Vec<i64> {
    use std::collections::HashMap;

    let mut sessions: HashMap<&str, i64> = HashMap::new();
    let mut loose: Vec<i64> = Vec::new();

    for (session_id, total) in rows {
        match session_id.as_deref() {
            None => loose.push(*total),
            Some(session) => {
                let slot = sessions.entry(session).or_insert(*total);
                *slot = (*slot).max(*total);
            }
        }
    }

    loose.extend(sessions.into_values());
    loose
}

/// PURE: the four gate outcomes, counted from the literals the núcleo writes.
///
/// An unrecognised literal counts as nothing at all rather than being swept into one of the four.
/// A new gate state added to the núcleo and forgotten here should show up as numbers that do not
/// add up to the run count — which is visible — rather than as a silent inflation of `errored`.
pub fn tally_gates(statuses: &[Option<String>]) -> Gate {
    let mut gate = Gate::default();
    for status in statuses {
        match status.as_deref() {
            None => gate.no_gate += 1,
            Some("passed") => gate.passed += 1,
            Some("failed") => gate.failed += 1,
            Some("errored") => gate.errored += 1,
            // `running` is a gate still going, which a finished-run window should not contain and
            // which is certainly not a verdict. Counted nowhere on purpose.
            Some(_) => {}
        }
    }
    gate
}

/// PURE: the middle value, or nothing when there is no population.
///
/// Through `Baseline`, which is the statistic `token_efficiency.rs` already uses on exactly these
/// numbers — median rather than mean because token totals are heavily skewed and a mean is dragged
/// by the outliers a reading like this exists to surface.
pub fn median_of(values: &[i64]) -> Option<i64> {
    Baseline::from_totals(values).median_total
}

/* ------------------------------------------------------------------ reads -- */

type TokenRow = (Option<String>, i64);

/// Session totals for one project's finished runs in `[since, until)`.
///
/// The three `IS NOT NULL` clauses are the same ones the efficiency baseline uses, and for the same
/// reason: they keep unmeasured runs out of the population instead of letting SQL turn silence into
/// a zero.
async fn token_rows(
    pool: &SqlitePool,
    project_id: &str,
    since: &str,
    until: &str,
) -> sqlx::Result<Vec<TokenRow>> {
    sqlx::query_as(
        "SELECT session_id, input_tokens + cache_read_tokens + cache_creation_tokens AS total \
           FROM runs \
          WHERE project_id = ? \
            AND completed_at IS NOT NULL AND completed_at >= ? AND completed_at < ? \
            AND input_tokens IS NOT NULL AND cache_read_tokens IS NOT NULL \
            AND cache_creation_tokens IS NOT NULL",
    )
    .bind(project_id)
    .bind(since)
    .bind(until)
    .fetch_all(pool)
    .await
}

/// Everything the four readings need, for one project over one window.
///
/// `now` is a parameter rather than read here so that a test can state its own window instead of
/// being written around the clock.
pub async fn readings(
    pool: &SqlitePool,
    project_id: &str,
    days: i64,
    now: DateTime<Utc>,
) -> sqlx::Result<Readings> {
    let days = days.clamp(1, MAX_WINDOW_DAYS);
    let since = now - Duration::days(days);
    let previous_since = since - Duration::days(days);

    let since_text = since.to_rfc3339();
    let now_text = now.to_rfc3339();
    let previous_since_text = previous_since.to_rfc3339();

    /* ------------------------------------------------------------ efficiency -- */

    let totals = session_totals(&token_rows(pool, project_id, &since_text, &now_text).await?);
    let previous =
        session_totals(&token_rows(pool, project_id, &previous_since_text, &since_text).await?);

    let unmeasured_runs: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM runs \
          WHERE project_id = ? \
            AND completed_at IS NOT NULL AND completed_at >= ? AND completed_at < ? \
            AND (input_tokens IS NULL OR cache_read_tokens IS NULL \
                 OR cache_creation_tokens IS NULL)",
    )
    .bind(project_id)
    .bind(&since_text)
    .bind(&now_text)
    .fetch_one(pool)
    .await?;

    let efficiency = Efficiency {
        measured_runs: totals.len() as i64,
        unmeasured_runs,
        median_total_tokens: median_of(&totals),
        previous_median_total_tokens: median_of(&previous),
    };

    /* ------------------------------------------------------------------ gate -- */

    let statuses: Vec<Option<String>> = sqlx::query_scalar(
        "SELECT gate_status FROM runs \
          WHERE project_id = ? \
            AND completed_at IS NOT NULL AND completed_at >= ? AND completed_at < ?",
    )
    .bind(project_id)
    .bind(&since_text)
    .bind(&now_text)
    .fetch_all(pool)
    .await?;
    let gate = tally_gates(&statuses);

    /* ------------------------------------------------------------------ cost -- */

    let cost = crate::budget::project_spend(pool, project_id, since, now).await?;

    /* ------------------------------------------------------------- delivered -- */

    let delivered = delivered(pool, project_id, &since_text, &now_text).await?;

    Ok(Readings {
        window_days: days,
        efficiency,
        cost,
        gate,
        delivered,
    })
}

/// What reached the integration branch, and how long it took to get there.
///
/// Landing is a **merge** in the VCS queue — `POST /vcs/land` enqueues one — so that is what is
/// counted, not a status on the run. The elapsed time is measured from the run that produced the
/// work to the merge finishing, because that is the number somebody wants: how long from starting
/// to shipped, not how long the queue held it.
async fn delivered(
    pool: &SqlitePool,
    project_id: &str,
    since: &str,
    until: &str,
) -> sqlx::Result<Delivered> {
    let landed: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM vcs_requests \
          WHERE project_id = ? AND op = 'merge' AND status = 'succeeded' \
            AND finished_at IS NOT NULL AND finished_at >= ? AND finished_at < ?",
    )
    .bind(project_id)
    .bind(since)
    .bind(until)
    .fetch_one(pool)
    .await?;

    let spans: Vec<(String, String)> = sqlx::query_as(
        "SELECT runs.created_at, vcs_requests.finished_at \
           FROM vcs_requests JOIN runs ON runs.id = vcs_requests.run_id \
          WHERE vcs_requests.project_id = ? AND vcs_requests.op = 'merge' \
            AND vcs_requests.status = 'succeeded' \
            AND vcs_requests.finished_at IS NOT NULL \
            AND vcs_requests.finished_at >= ? AND vcs_requests.finished_at < ?",
    )
    .bind(project_id)
    .bind(since)
    .bind(until)
    .fetch_all(pool)
    .await?;

    let minutes: Vec<i64> = spans
        .iter()
        .filter_map(|(started, finished)| {
            let started = DateTime::parse_from_rfc3339(started).ok()?;
            let finished = DateTime::parse_from_rfc3339(finished).ok()?;
            // A negative span is a clock that moved, not a delivery that arrived before it began.
            // Dropped rather than clamped to zero: an impossible duration is not a fast one.
            let minutes = (finished - started).num_minutes();
            (minutes >= 0).then_some(minutes)
        })
        .collect();

    Ok(Delivered {
        landed,
        timed: minutes.len() as i64,
        median_minutes: median_of(&minutes),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_resumed_session_is_counted_once_at_its_largest() {
        // Three rows of one session: the CLI reporting a running total as the run resumed.
        let rows = vec![
            (Some("s1".to_string()), 10_000),
            (Some("s1".to_string()), 25_000),
            (Some("s1".to_string()), 40_000),
        ];
        assert_eq!(session_totals(&rows), vec![40_000]);
    }

    #[test]
    fn a_session_that_reports_less_than_it_already_had_does_not_erase_the_larger_figure() {
        // A restart or a version change can report backwards. Taking the last value would forget
        // tokens that were really spent; `budget.rs` rounds the same way for the same reason.
        let rows = vec![
            (Some("s1".to_string()), 40_000),
            (Some("s1".to_string()), 9_000),
        ];
        assert_eq!(session_totals(&rows), vec![40_000]);
    }

    #[test]
    fn a_run_with_no_session_cannot_be_a_resumption_and_counts_on_its_own() {
        let rows = vec![(None, 1_000), (None, 2_000)];
        let mut totals = session_totals(&rows);
        totals.sort_unstable();
        assert_eq!(totals, vec![1_000, 2_000]);
    }

    #[test]
    fn the_three_gate_verdicts_never_merge_and_no_gate_is_a_fourth_thing() {
        let statuses = vec![
            Some("passed".to_string()),
            Some("passed".to_string()),
            Some("failed".to_string()),
            Some("errored".to_string()),
            None,
            None,
            None,
        ];
        let gate = tally_gates(&statuses);
        assert_eq!(gate.passed, 2);
        assert_eq!(gate.failed, 1);
        assert_eq!(gate.errored, 1);
        assert_eq!(gate.no_gate, 3);
    }

    #[test]
    fn a_gate_state_this_does_not_know_is_counted_nowhere_rather_than_guessed_at() {
        // Numbers that fail to add up are visible; a silently inflated `errored` is not.
        let gate = tally_gates(&[Some("running".to_string()), Some("moonwalking".to_string())]);
        assert_eq!(gate, Gate::default());
    }

    #[test]
    fn a_median_of_nothing_is_nothing_rather_than_zero() {
        assert_eq!(median_of(&[]), None);
        assert_eq!(median_of(&[7]), Some(7));
        assert_eq!(median_of(&[1, 3]), Some(2));
    }

    async fn test_pool() -> SqlitePool {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!().run(&pool).await.unwrap();
        pool
    }

    /// A finished run, with whatever the caller wants it to have reported.
    #[allow(clippy::too_many_arguments)]
    async fn insert_run(
        pool: &SqlitePool,
        project: &str,
        completed_at: DateTime<Utc>,
        session: Option<&str>,
        tokens: Option<i64>,
        gate: Option<&str>,
        cost: Option<f64>,
    ) -> i64 {
        sqlx::query_scalar(
            "INSERT INTO runs (prompt, status, mode, project_id, session_id, created_at, \
             completed_at, input_tokens, cache_read_tokens, cache_creation_tokens, gate_status, \
             cost_usd) \
             VALUES ('p', 'completed', 'worktree', ?, ?, ?, ?, ?, 0, 0, ?, ?) RETURNING id",
        )
        .bind(project)
        .bind(session)
        .bind(completed_at.to_rfc3339())
        .bind(completed_at.to_rfc3339())
        .bind(tokens)
        .bind(gate)
        .bind(cost)
        .fetch_one(pool)
        .await
        .unwrap()
    }

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-08-23T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    #[tokio::test]
    async fn a_run_outside_the_window_is_not_in_the_reading() {
        let pool = test_pool().await;
        let now = now();
        insert_run(
            &pool,
            "alpha",
            now - Duration::days(2),
            None,
            Some(1_000),
            Some("passed"),
            None,
        )
        .await;
        insert_run(
            &pool,
            "alpha",
            now - Duration::days(40),
            None,
            Some(9_000),
            Some("failed"),
            None,
        )
        .await;

        let readings = readings(&pool, "alpha", 30, now).await.unwrap();
        assert_eq!(readings.efficiency.measured_runs, 1);
        assert_eq!(readings.gate.passed, 1);
        // The 40-day-old failure is not in this window and must not be counted into it...
        assert_eq!(readings.gate.failed, 0);
        // ...but it IS the window before, which is what the comparison is for.
        assert_eq!(
            readings.efficiency.previous_median_total_tokens,
            Some(9_000)
        );
    }

    #[tokio::test]
    async fn another_project_is_not_in_this_project_s_reading() {
        let pool = test_pool().await;
        let now = now();
        insert_run(
            &pool,
            "alpha",
            now - Duration::days(1),
            None,
            Some(1_000),
            Some("passed"),
            None,
        )
        .await;
        insert_run(
            &pool,
            "beta",
            now - Duration::days(1),
            None,
            Some(50_000),
            Some("failed"),
            None,
        )
        .await;

        let readings = readings(&pool, "alpha", 30, now).await.unwrap();
        assert_eq!(readings.efficiency.median_total_tokens, Some(1_000));
        assert_eq!(readings.gate.failed, 0);
    }

    #[tokio::test]
    async fn a_run_that_reported_nothing_is_counted_apart_and_never_as_a_zero() {
        let pool = test_pool().await;
        let now = now();
        insert_run(
            &pool,
            "alpha",
            now - Duration::days(1),
            None,
            Some(80_000),
            None,
            None,
        )
        .await;
        insert_run(
            &pool,
            "alpha",
            now - Duration::days(1),
            None,
            None,
            None,
            None,
        )
        .await;
        insert_run(
            &pool,
            "alpha",
            now - Duration::days(1),
            None,
            None,
            None,
            None,
        )
        .await;

        let readings = readings(&pool, "alpha", 30, now).await.unwrap();
        assert_eq!(readings.efficiency.measured_runs, 1);
        assert_eq!(readings.efficiency.unmeasured_runs, 2);
        // Averaging the two silences in as zeros would put this near 27,000.
        assert_eq!(readings.efficiency.median_total_tokens, Some(80_000));
    }

    #[tokio::test]
    async fn a_resumed_session_does_not_multiply_the_median() {
        let pool = test_pool().await;
        let now = now();
        for total in [20_000, 45_000, 70_000] {
            insert_run(
                &pool,
                "alpha",
                now - Duration::days(1),
                Some("s1"),
                Some(total),
                None,
                None,
            )
            .await;
        }

        let readings = readings(&pool, "alpha", 30, now).await.unwrap();
        // One session, so one measurement — not three.
        assert_eq!(readings.efficiency.measured_runs, 1);
        assert_eq!(readings.efficiency.median_total_tokens, Some(70_000));
    }

    #[tokio::test]
    async fn a_project_with_nothing_in_the_window_reports_absence_rather_than_zero() {
        let pool = test_pool().await;

        let readings = readings(&pool, "empty", 30, now()).await.unwrap();
        assert_eq!(readings.efficiency.measured_runs, 0);
        assert_eq!(readings.efficiency.median_total_tokens, None);
        assert_eq!(readings.delivered.median_minutes, None);
        assert_eq!(readings.gate, Gate::default());
    }

    #[tokio::test]
    async fn the_window_is_bounded_so_a_reading_cannot_become_a_report() {
        let pool = test_pool().await;
        let widest = readings(&pool, "alpha", 100_000, now()).await.unwrap();
        assert_eq!(widest.window_days, MAX_WINDOW_DAYS);

        let narrowest = readings(&pool, "alpha", 0, now()).await.unwrap();
        assert_eq!(narrowest.window_days, 1);
    }

    async fn insert_merge(
        pool: &SqlitePool,
        project: &str,
        status: &str,
        run_id: Option<i64>,
        finished_at: DateTime<Utc>,
    ) {
        sqlx::query(
            "INSERT INTO vcs_requests (op, args, project_id, project_root, repo_key, origin, \
             run_id, status, created_at, finished_at) \
             VALUES ('merge', '{}', ?, 'C:/repo', 'k', 'run', ?, ?, ?, ?)",
        )
        .bind(project)
        .bind(run_id)
        .bind(status)
        .bind(finished_at.to_rfc3339())
        .bind(finished_at.to_rfc3339())
        .execute(pool)
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn only_a_merge_that_succeeded_counts_as_delivered() {
        let pool = test_pool().await;
        let now = now();
        insert_merge(&pool, "alpha", "succeeded", None, now - Duration::days(1)).await;
        insert_merge(&pool, "alpha", "failed", None, now - Duration::days(1)).await;
        insert_merge(&pool, "alpha", "escalated", None, now - Duration::days(1)).await;

        let readings = readings(&pool, "alpha", 30, now).await.unwrap();
        assert_eq!(readings.delivered.landed, 1);
    }

    #[tokio::test]
    async fn a_merge_with_no_run_behind_it_is_counted_but_not_timed() {
        let pool = test_pool().await;
        let now = now();

        let run = insert_run(
            &pool,
            "alpha",
            now - Duration::days(1),
            None,
            None,
            None,
            None,
        )
        .await;
        // Its run started two hours before the merge finished.
        sqlx::query("UPDATE runs SET created_at = ? WHERE id = ?")
            .bind((now - Duration::days(1) - Duration::hours(2)).to_rfc3339())
            .bind(run)
            .execute(&pool)
            .await
            .unwrap();
        insert_merge(
            &pool,
            "alpha",
            "succeeded",
            Some(run),
            now - Duration::days(1),
        )
        .await;
        // And one somebody asked for by hand, with no run to measure from.
        insert_merge(&pool, "alpha", "succeeded", None, now - Duration::days(1)).await;

        let readings = readings(&pool, "alpha", 30, now).await.unwrap();
        assert_eq!(readings.delivered.landed, 2);
        // Visible as a median over one of two, rather than silently being one.
        assert_eq!(readings.delivered.timed, 1);
        assert_eq!(readings.delivered.median_minutes, Some(120));
    }
}
