//! Metric 10's read (spec 2026-10-17-analytics-devtime §3 and §5.1, `agent_rows`): one row per
//! agent attempt (`kind = 'agent'`) in the window, with its outcome, background status, mark verdict
//! and duration, for `analytics/devtime/agent_effectiveness.rs` to assemble. Read-only.
//!
//! Takes `since`/`until` stamps in the one devtime timestamp format (text order is time order) and
//! an optional project. The project lives on `devtime_sessions` only, so the filter is a join
//! through it. Migration 0255's `idx_devtime_attempts_day` serves the window's range and the mark is
//! joined by its primary key, so no table is scanned. No id column is selected, so none can leave
//! the daemon (spec §8).

use sqlx::SqlitePool;

/// One agent attempt in the window.
///
/// `verified` is the attempt mark's verdict; `None` means the session has had no rules pass yet
/// (every agent attempt of a ruled session is marked, `rules/project.rs`). `ms` runs from
/// `started_at` to the background end when there is one, otherwise to `ended_at`; `None` means the
/// attempt is still open, including a launched background task that has not ended yet. The tokens are the store's as written; folding unknown ones is the
/// assembler's.
#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct AgentAttemptRow {
    pub agent_type: Option<String>,
    pub outcome: String,
    pub bg_status: Option<String>,
    pub verified: Option<String>,
    pub ms: Option<i64>,
}

macro_rules! agent_attempt_sql {
    ($project:literal) => {
        concat!(
            "SELECT a.agent_type, a.outcome, a.bg_status, m.verified,
                    CASE WHEN a.outcome = 'launched' AND a.bg_ended_at IS NULL THEN NULL
                         ELSE CAST(ROUND((julianday(COALESCE(a.bg_ended_at, a.ended_at))
                                          - julianday(a.started_at)) * 86400000.0) AS INTEGER)
                    END AS ms
             FROM devtime_attempts a
             CROSS JOIN devtime_sessions ss ON ss.session_id = a.session_id
             LEFT JOIN devtime_attempt_marks m ON m.attempt_id = a.attempt_id
             WHERE a.started_at >= ?1 AND a.started_at < ?2 AND a.kind = 'agent'",
            $project,
            "
             ORDER BY a.started_at, a.attempt_id"
        )
    };
}

/// The global form of [`agent_rows`]: binds `?1` since and `?2` until.
pub(crate) const AGENT_ATTEMPT_SQL: &str = agent_attempt_sql!("");
/// The project form of [`agent_rows`]: also binds `?3` the project id.
pub(crate) const AGENT_ATTEMPT_PROJECT_SQL: &str = agent_attempt_sql!(" AND ss.project_id = ?3");

/// The current window's agent attempts (`since <= started_at < until`), one row each, for one
/// project or all of them. Rows come in start order; there is no previous-window counterpart.
pub async fn agent_rows(
    pool: &SqlitePool,
    project_id: Option<&str>,
    since: &str,
    until: &str,
) -> sqlx::Result<Vec<AgentAttemptRow>> {
    let sql = match project_id {
        Some(_) => AGENT_ATTEMPT_PROJECT_SQL,
        None => AGENT_ATTEMPT_SQL,
    };
    let query = sqlx::query_as::<_, AgentAttemptRow>(sql).bind(since);
    match project_id {
        Some(project) => query.bind(until).bind(project).fetch_all(pool).await,
        None => query.bind(until).fetch_all(pool).await,
    }
}

#[cfg(test)]
pub(crate) mod agent_fixture {
    use sqlx::SqlitePool;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// One agent `devtime_attempts` row, with only the columns metric 10 reads set by the caller.
    pub(crate) struct AgentAttempt<'a> {
        pub session: &'a str,
        pub agent_type: Option<&'a str>,
        pub started_at: &'a str,
        pub ended_at: Option<&'a str>,
        pub outcome: &'a str,
        pub bg_status: Option<&'a str>,
        pub bg_ended_at: Option<&'a str>,
        pub lane: &'a str,
    }

    /// A successful, still open agent attempt on the main lane, with no background task.
    pub(crate) fn agent<'a>(
        session: &'a str,
        agent_type: Option<&'a str>,
        started_at: &'a str,
    ) -> AgentAttempt<'a> {
        AgentAttempt {
            session,
            agent_type,
            started_at,
            ended_at: None,
            outcome: "ok",
            bg_status: None,
            bg_ended_at: None,
            lane: "main",
        }
    }

    /// Makes every inserted attempt's id and tool-use id unique, whatever its other columns.
    static NEXT: AtomicUsize = AtomicUsize::new(0);

    /// Inserts the attempt and returns its id, for [`insert_mark`].
    pub(crate) async fn insert_agent(pool: &SqlitePool, a: AgentAttempt<'_>) -> String {
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let attempt_id = format!("agent:{}:{}:{n}", a.session, a.started_at);
        let tool_use_id = format!("toolu_agent_{n}");
        sqlx::query(
            "INSERT INTO devtime_attempts (attempt_id, session_id, lane, tool_use_id, kind, tool_name,
                                           agent_type, started_at, ended_at, outcome, bg_status,
                                           bg_ended_at, parser_version)
             VALUES (?, ?, ?, ?, 'agent', 'Agent', ?, ?, ?, ?, ?, ?, 2)",
        )
        .bind(&attempt_id)
        .bind(a.session)
        .bind(a.lane)
        .bind(tool_use_id)
        .bind(a.agent_type)
        .bind(a.started_at)
        .bind(a.ended_at)
        .bind(a.outcome)
        .bind(a.bg_status)
        .bind(a.bg_ended_at)
        .execute(pool)
        .await
        .unwrap();
        attempt_id
    }

    /// Marks the attempt with a verdict, as a rules pass would.
    pub(crate) async fn insert_mark(
        pool: &SqlitePool,
        attempt_id: &str,
        session: &str,
        verified: &str,
    ) {
        sqlx::query(
            "INSERT INTO devtime_attempt_marks (attempt_id, session_id, verified, rules_version)
             VALUES (?, ?, ?, 'v1')",
        )
        .bind(attempt_id)
        .bind(session)
        .bind(verified)
        .execute(pool)
        .await
        .unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::agent_fixture::*;
    use super::*;
    use crate::devtime::store::attempt_fixture::{attempt, insert_attempt};
    use crate::devtime::store::fixture::session;

    const S: &str = "2026-10-01T00:00:00.000Z";
    const U: &str = "2026-10-18T00:00:00.000Z";
    const IN: &str = "2026-10-05T10:00:00.000Z";

    async fn pool() -> SqlitePool {
        crate::testdb::fresh_pool().await
    }

    fn row(
        agent_type: Option<&str>,
        outcome: &str,
        bg_status: Option<&str>,
        verified: Option<&str>,
        ms: Option<i64>,
    ) -> AgentAttemptRow {
        AgentAttemptRow {
            agent_type: agent_type.map(str::to_owned),
            outcome: outcome.to_owned(),
            bg_status: bg_status.map(str::to_owned),
            verified: verified.map(str::to_owned),
            ms,
        }
    }

    /// The read's rows in a fixed order independent of the attempt ids.
    async fn rows(pool: &SqlitePool, project: Option<&str>) -> Vec<AgentAttemptRow> {
        let mut rows = agent_rows(pool, project, S, U).await.unwrap();
        rows.sort_by(|a, b| {
            (&a.agent_type, &a.outcome, &a.verified, &a.ms).cmp(&(
                &b.agent_type,
                &b.outcome,
                &b.verified,
                &b.ms,
            ))
        });
        rows
    }

    #[tokio::test]
    async fn agent_rows_read_agent_attempts_only() {
        let pool = pool().await;
        session(&pool, "s1", "alpha", S, None).await;
        insert_attempt(&pool, attempt("s1", "Bash", IN)).await;
        insert_agent(&pool, agent("s1", Some("Explore"), IN)).await;

        assert_eq!(
            rows(&pool, None).await,
            vec![row(Some("Explore"), "ok", None, None, None)]
        );
    }

    #[tokio::test]
    async fn agent_rows_join_the_mark_or_none() {
        let pool = pool().await;
        session(&pool, "s1", "alpha", S, None).await;
        let marked = insert_agent(&pool, agent("s1", Some("wf-executor"), IN)).await;
        insert_agent(
            &pool,
            agent("s1", Some("Explore"), "2026-10-05T11:00:00.000Z"),
        )
        .await;
        insert_mark(&pool, &marked, "s1", "passed").await;

        assert_eq!(
            rows(&pool, None).await,
            vec![
                row(Some("Explore"), "ok", None, None, None),
                row(Some("wf-executor"), "ok", None, Some("passed"), None),
            ]
        );
    }

    #[tokio::test]
    async fn agent_ms_runs_to_the_background_end_when_there_is_one() {
        let pool = pool().await;
        session(&pool, "s1", "alpha", S, None).await;
        let foreground = AgentAttempt {
            ended_at: Some("2026-10-05T10:02:00.000Z"),
            ..agent("s1", Some("fg"), "2026-10-05T10:00:00.000Z")
        };
        insert_agent(&pool, foreground).await;
        let background = AgentAttempt {
            ended_at: Some("2026-10-05T10:00:01.000Z"),
            outcome: "launched",
            bg_status: Some("completed"),
            bg_ended_at: Some("2026-10-05T10:05:00.000Z"),
            ..agent("s1", Some("bg"), "2026-10-05T10:00:00.000Z")
        };
        insert_agent(&pool, background).await;
        insert_agent(&pool, agent("s1", Some("open"), "2026-10-05T10:00:00.000Z")).await;

        assert_eq!(
            rows(&pool, None).await,
            vec![
                row(
                    Some("bg"),
                    "launched",
                    Some("completed"),
                    None,
                    Some(300_000)
                ),
                row(Some("fg"), "ok", None, None, Some(120_000)),
                row(Some("open"), "ok", None, None, None),
            ]
        );
    }

    /// A background agent still running has only its launch time behind `ended_at`; counting
    /// that as its duration would skew `ms` and the median, so it reads as open (`None`).
    #[tokio::test]
    async fn agent_ms_is_none_while_the_background_task_runs() {
        let pool = pool().await;
        session(&pool, "s1", "alpha", S, None).await;
        let running = AgentAttempt {
            ended_at: Some("2026-10-05T10:00:01.000Z"),
            outcome: "launched",
            ..agent("s1", Some("bg"), "2026-10-05T10:00:00.000Z")
        };
        insert_agent(&pool, running).await;

        assert_eq!(
            rows(&pool, None).await,
            vec![row(Some("bg"), "launched", None, None, None)]
        );
    }

    #[tokio::test]
    async fn window_is_half_open_on_started_at() {
        let pool = pool().await;
        session(&pool, "s1", "alpha", "2026-09-30T00:00:00.000Z", None).await;
        insert_agent(&pool, agent("s1", Some("in"), S)).await;
        insert_agent(&pool, agent("s1", Some("at_until"), U)).await;
        insert_agent(
            &pool,
            agent("s1", Some("before"), "2026-09-30T23:59:59.999Z"),
        )
        .await;

        assert_eq!(
            rows(&pool, None).await,
            vec![row(Some("in"), "ok", None, None, None)]
        );
    }

    /// Two agent attempts in `alpha`, one in `beta`.
    async fn two_projects(pool: &SqlitePool) {
        session(pool, "a1", "alpha", S, None).await;
        session(pool, "b1", "beta", S, None).await;
        insert_agent(pool, agent("a1", Some("Explore"), IN)).await;
        insert_agent(
            pool,
            agent("a1", Some("Explore"), "2026-10-06T10:00:00.000Z"),
        )
        .await;
        insert_agent(pool, agent("b1", Some("Explore"), IN)).await;
    }

    #[tokio::test]
    async fn project_filter_excludes_other_projects() {
        let pool = pool().await;
        two_projects(&pool).await;

        assert_eq!(rows(&pool, Some("alpha")).await.len(), 2);
        assert_eq!(rows(&pool, Some("beta")).await.len(), 1);
    }

    #[tokio::test]
    async fn none_project_sums_all() {
        let pool = pool().await;
        two_projects(&pool).await;

        assert_eq!(rows(&pool, None).await.len(), 3);
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

    /// Spec §9 (performance): both forms take their rows through migration 0255's index, and no
    /// table is scanned.
    #[tokio::test]
    async fn the_agent_reads_take_rows_from_the_day_index() {
        let pool = pool().await;
        for (sql, project) in [
            (AGENT_ATTEMPT_SQL, false),
            (AGENT_ATTEMPT_PROJECT_SQL, true),
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
