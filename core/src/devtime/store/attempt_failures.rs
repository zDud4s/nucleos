//! Metric 7's read (spec 2026-10-17-analytics-devtime §3 and §5.1): the window's tool attempts
//! grouped by tool, program, outcome and failure class, for `analytics/devtime/tool_failures.rs`
//! to assemble. Read-only.
//!
//! Takes `since`/`until` stamps in the one devtime timestamp format (text order is time order) and
//! an optional project. The project lives on `devtime_sessions` only, so the filter is a join
//! through it. Migration 0255's `idx_devtime_attempts_day` covers the read, so no table is scanned.

use sqlx::SqlitePool;

/// Attempts of one combination of tool, raw program, outcome and failure class in the window.
///
/// `failure_class` is `background_failed` / `background_killed` for a `launched` attempt whose
/// background task ended that way (whatever its stored `error_class`), otherwise the stored
/// `error_class` when there is one, otherwise `None`. The tokens are the store's as written; mapping unknown ones is the assembler's.
#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct AttemptFailureRow {
    pub tool_name: String,
    pub cmd_program: Option<String>,
    pub outcome: String,
    pub failure_class: Option<String>,
    pub attempts: i64,
}

macro_rules! attempt_failure_sql {
    ($project:literal) => {
        concat!(
            "SELECT a.tool_name, a.cmd_program, a.outcome,
                    CASE WHEN a.outcome = 'launched' AND a.bg_status = 'failed' THEN 'background_failed'
                         WHEN a.outcome = 'launched' AND a.bg_status = 'killed' THEN 'background_killed'
                         WHEN a.error_class IS NOT NULL THEN a.error_class
                    END AS failure_class,
                    COUNT(*) AS attempts
             FROM devtime_attempts a
             CROSS JOIN devtime_sessions ss ON ss.session_id = a.session_id
             WHERE a.started_at >= ?1 AND a.started_at < ?2",
            $project,
            "
             GROUP BY a.tool_name, a.cmd_program, a.outcome, failure_class"
        )
    };
}

/// The global form of [`attempt_failure_rows`]: binds `?1` since and `?2` until.
pub(crate) const ATTEMPT_FAILURE_SQL: &str = attempt_failure_sql!("");
/// The project form of [`attempt_failure_rows`]: also binds `?3` the project id.
pub(crate) const ATTEMPT_FAILURE_PROJECT_SQL: &str =
    attempt_failure_sql!(" AND ss.project_id = ?3");

/// The current window's tool attempts (`since <= started_at < until`), grouped by tool, program,
/// outcome and failure class, for one project or all of them. The window is the current one only:
/// there is no previous-window counterpart. `cmd_program` is raw, exactly as the parser stored it;
/// redacting it is the assembler's job (spec §8).
pub async fn attempt_failure_rows(
    pool: &SqlitePool,
    project_id: Option<&str>,
    since: &str,
    until: &str,
) -> sqlx::Result<Vec<AttemptFailureRow>> {
    let sql = match project_id {
        Some(_) => ATTEMPT_FAILURE_PROJECT_SQL,
        None => ATTEMPT_FAILURE_SQL,
    };
    let query = sqlx::query_as::<_, AttemptFailureRow>(sql).bind(since);
    match project_id {
        Some(project) => query.bind(until).bind(project).fetch_all(pool).await,
        None => query.bind(until).fetch_all(pool).await,
    }
}

#[cfg(test)]
pub(crate) mod attempt_fixture {
    use sqlx::SqlitePool;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// One `devtime_attempts` row, with only the columns metric 7 reads set by the caller.
    pub(crate) struct Attempt<'a> {
        pub session: &'a str,
        pub tool_name: &'a str,
        pub cmd_program: Option<&'a str>,
        pub started_at: &'a str,
        pub outcome: &'a str,
        pub error_class: Option<&'a str>,
        pub bg_status: Option<&'a str>,
        pub lane: &'a str,
    }

    /// A successful attempt on the main lane, with no program, class or background status.
    pub(crate) fn attempt<'a>(
        session: &'a str,
        tool_name: &'a str,
        started_at: &'a str,
    ) -> Attempt<'a> {
        Attempt {
            session,
            tool_name,
            cmd_program: None,
            started_at,
            outcome: "ok",
            error_class: None,
            bg_status: None,
            lane: "main",
        }
    }

    /// Makes every inserted attempt's id and tool-use id unique, whatever its other columns.
    static NEXT: AtomicUsize = AtomicUsize::new(0);

    pub(crate) async fn insert_attempt(pool: &SqlitePool, a: Attempt<'_>) {
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let attempt_id = format!("{}:{}:{n}", a.session, a.started_at);
        let tool_use_id = format!("toolu_{n}");
        sqlx::query(
            "INSERT INTO devtime_attempts (attempt_id, session_id, lane, tool_use_id, kind, tool_name,
                                           started_at, outcome, error_class, cmd_program, bg_status,
                                           parser_version)
             VALUES (?, ?, ?, ?, 'tool', ?, ?, ?, ?, ?, ?, 2)",
        )
        .bind(attempt_id)
        .bind(a.session)
        .bind(a.lane)
        .bind(tool_use_id)
        .bind(a.tool_name)
        .bind(a.started_at)
        .bind(a.outcome)
        .bind(a.error_class)
        .bind(a.cmd_program)
        .bind(a.bg_status)
        .execute(pool)
        .await
        .unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::attempt_fixture::*;
    use super::*;
    use crate::devtime::store::fixture::session;

    const S: &str = "2026-10-01T00:00:00.000Z";
    const U: &str = "2026-10-18T00:00:00.000Z";
    const IN: &str = "2026-10-05T10:00:00.000Z";

    async fn pool() -> SqlitePool {
        crate::testdb::fresh_pool().await
    }

    fn row(
        tool: &str,
        program: Option<&str>,
        outcome: &str,
        class: Option<&str>,
        attempts: i64,
    ) -> AttemptFailureRow {
        AttemptFailureRow {
            tool_name: tool.to_owned(),
            cmd_program: program.map(str::to_owned),
            outcome: outcome.to_owned(),
            failure_class: class.map(str::to_owned),
            attempts,
        }
    }

    /// The read's rows in a fixed order, since GROUP BY promises none.
    async fn rows(pool: &SqlitePool, project: Option<&str>) -> Vec<AttemptFailureRow> {
        let mut rows = attempt_failure_rows(pool, project, S, U).await.unwrap();
        rows.sort_by(|a, b| {
            (&a.tool_name, &a.cmd_program, &a.outcome, &a.failure_class).cmp(&(
                &b.tool_name,
                &b.cmd_program,
                &b.outcome,
                &b.failure_class,
            ))
        });
        rows
    }

    fn cargo_error<'a>(session: &'a str, at: &'a str) -> Attempt<'a> {
        Attempt {
            cmd_program: Some("cargo test"),
            outcome: "error",
            error_class: Some("exit_nonzero"),
            ..attempt(session, "Bash", at)
        }
    }

    #[tokio::test]
    async fn attempt_failure_rows_group_by_tool_program_outcome_and_class() {
        let pool = pool().await;
        session(&pool, "s1", "alpha", S, None).await;
        insert_attempt(&pool, cargo_error("s1", "2026-10-05T10:00:00.000Z")).await;
        insert_attempt(&pool, cargo_error("s1", "2026-10-05T10:01:00.000Z")).await;
        let ok = Attempt {
            cmd_program: Some("cargo test"),
            ..attempt("s1", "Bash", "2026-10-05T10:02:00.000Z")
        };
        insert_attempt(&pool, ok).await;
        let edit = Attempt {
            outcome: "error",
            error_class: Some("edit_not_found"),
            ..attempt("s1", "Edit", "2026-10-05T10:03:00.000Z")
        };
        insert_attempt(&pool, edit).await;

        assert_eq!(
            rows(&pool, None).await,
            vec![
                row("Bash", Some("cargo test"), "error", Some("exit_nonzero"), 2),
                row("Bash", Some("cargo test"), "ok", None, 1),
                row("Edit", None, "error", Some("edit_not_found"), 1),
            ]
        );
    }

    #[tokio::test]
    async fn background_failure_is_classed_from_bg_status() {
        let pool = pool().await;
        session(&pool, "s1", "alpha", S, None).await;
        for (minute, status) in [("10", "failed"), ("11", "killed"), ("12", "completed")] {
            let at = format!("2026-10-05T10:{minute}:00.000Z");
            let launched = Attempt {
                outcome: "launched",
                bg_status: Some(status),
                ..attempt("s1", "Bash", &at)
            };
            insert_attempt(&pool, launched).await;
        }

        assert_eq!(
            rows(&pool, None).await,
            vec![
                row("Bash", None, "launched", None, 1),
                row("Bash", None, "launched", Some("background_failed"), 1),
                row("Bash", None, "launched", Some("background_killed"), 1),
            ]
        );
    }

    #[tokio::test]
    async fn launched_bg_status_wins_over_a_stored_error_class() {
        let pool = pool().await;
        session(&pool, "s1", "alpha", S, None).await;
        for (minute, status) in [("10", "failed"), ("11", "killed"), ("12", "completed")] {
            let at = format!("2026-10-05T10:{minute}:00.000Z");
            let launched = Attempt {
                outcome: "launched",
                error_class: Some("exit_nonzero"),
                bg_status: Some(status),
                ..attempt("s1", "Bash", &at)
            };
            insert_attempt(&pool, launched).await;
        }

        assert_eq!(
            rows(&pool, None).await,
            vec![
                row("Bash", None, "launched", Some("background_failed"), 1),
                row("Bash", None, "launched", Some("background_killed"), 1),
                row("Bash", None, "launched", Some("exit_nonzero"), 1),
            ]
        );
    }

    #[tokio::test]
    async fn window_is_half_open_on_started_at() {
        let pool = pool().await;
        session(&pool, "s1", "alpha", "2026-09-30T00:00:00.000Z", None).await;
        insert_attempt(&pool, attempt("s1", "Read", S)).await;
        insert_attempt(&pool, attempt("s1", "Read", U)).await;
        insert_attempt(&pool, attempt("s1", "Read", "2026-09-30T23:59:59.999Z")).await;

        assert_eq!(
            rows(&pool, None).await,
            vec![row("Read", None, "ok", None, 1)]
        );
    }

    /// Two attempts in `alpha`, one in `beta`.
    async fn two_projects(pool: &SqlitePool) {
        session(pool, "a1", "alpha", S, None).await;
        session(pool, "b1", "beta", S, None).await;
        insert_attempt(pool, attempt("a1", "Read", IN)).await;
        insert_attempt(pool, attempt("a1", "Read", "2026-10-06T10:00:00.000Z")).await;
        insert_attempt(pool, attempt("b1", "Read", IN)).await;
    }

    #[tokio::test]
    async fn project_filter_excludes_other_projects() {
        let pool = pool().await;
        two_projects(&pool).await;

        assert_eq!(
            rows(&pool, Some("alpha")).await,
            vec![row("Read", None, "ok", None, 2)]
        );
        assert_eq!(
            rows(&pool, Some("beta")).await,
            vec![row("Read", None, "ok", None, 1)]
        );
    }

    #[tokio::test]
    async fn none_project_sums_all() {
        let pool = pool().await;
        two_projects(&pool).await;

        assert_eq!(
            rows(&pool, None).await,
            vec![row("Read", None, "ok", None, 3)]
        );
    }

    /// `EXPLAIN QUERY PLAN` of a read over the test window, for `alpha` when `project` (the project
    /// form binds `?3`, the global one does not), one `detail` per step.
    async fn read_plan(pool: &SqlitePool, sql: &str, project: bool) -> Vec<String> {
        use sqlx::Row as _;
        let query = sqlx::query(sqlx::AssertSqlSafe(format!("EXPLAIN QUERY PLAN {sql}")))
            .bind(S)
            .bind(U);
        let query = if project { query.bind("alpha") } else { query };
        let rows = query.fetch_all(pool).await.unwrap();
        rows.iter()
            .map(|row| row.get::<String, _>("detail"))
            .collect()
    }

    /// Spec §9 (performance): both forms take their rows from migration 0255's covering index,
    /// and no table is scanned.
    #[tokio::test]
    async fn the_attempt_reads_take_rows_from_the_day_index() {
        let pool = pool().await;
        for (sql, project) in [
            (ATTEMPT_FAILURE_SQL, false),
            (ATTEMPT_FAILURE_PROJECT_SQL, true),
        ] {
            let plan = read_plan(&pool, sql, project).await;
            assert!(
                plan.iter()
                    .any(|line| line.contains("idx_devtime_attempts_day")),
                "{plan:?}"
            );
            assert!(
                !plan.iter().any(|line| line.starts_with("SCAN ")),
                "{plan:?}"
            );
        }
    }
}
