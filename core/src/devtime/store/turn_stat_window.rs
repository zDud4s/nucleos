//! Metric 9's read (spec 2026-10-17-analytics-devtime §3 and §5.1): the stored per-turn stats whose
//! start falls in a window, for `analytics/devtime/turn_efficiency.rs` to fold into the per-class
//! table and the slow turns no rule explains. Read-only.
//!
//! Takes `since`/`until` stamps in the one devtime timestamp format (text order is time order) and
//! an optional project. `project_id` lives on the row itself (the rules pass copies it from the
//! session), so no join is needed. Migration 0256's `idx_devtime_turn_stats_day` serves the global
//! form, and 0171's `(project_id, turn_class, started_at)` index can serve the project form, so no
//! table is scanned.

use sqlx::SqlitePool;

use super::TurnStatRow;

macro_rules! turn_stat_window_sql {
    ($project:literal) => {
        concat!(
            "SELECT session_id, turn_seq, project_id, turn_class, started_at, calls, active_ms,
                    explained_ms
             FROM devtime_turn_stats
             WHERE started_at >= ?1 AND started_at < ?2",
            $project,
            "
             ORDER BY started_at, session_id, turn_seq"
        )
    };
}

/// The global form of [`turn_stat_window_rows`]: binds `?1` since and `?2` until.
pub(crate) const TURN_STAT_WINDOW_SQL: &str = turn_stat_window_sql!("");
/// The project form of [`turn_stat_window_rows`]: also binds `?3` the project id.
pub(crate) const TURN_STAT_WINDOW_PROJECT_SQL: &str = turn_stat_window_sql!(" AND project_id = ?3");

/// The stored turn stats with `since <= started_at < until`, for one project or all of them, in
/// start order (then session and turn, so the order is total).
pub async fn turn_stat_window_rows(
    pool: &SqlitePool,
    project_id: Option<&str>,
    since: &str,
    until: &str,
) -> sqlx::Result<Vec<TurnStatRow>> {
    let sql = match project_id {
        Some(_) => TURN_STAT_WINDOW_PROJECT_SQL,
        None => TURN_STAT_WINDOW_SQL,
    };
    let query = sqlx::query_as::<_, TurnStatRow>(sql).bind(since);
    match project_id {
        Some(project) => query.bind(until).bind(project).fetch_all(pool).await,
        None => query.bind(until).fetch_all(pool).await,
    }
}

#[cfg(test)]
pub(crate) mod turn_stat_fixture {
    use super::TurnStatRow;
    use sqlx::SqlitePool;

    /// A turn stat with 3 calls and nothing explained.
    pub(crate) fn stat(
        session: &str,
        seq: i64,
        project: &str,
        class: &str,
        started_at: &str,
        active_ms: i64,
    ) -> TurnStatRow {
        TurnStatRow {
            session_id: session.to_owned(),
            turn_seq: seq,
            project_id: project.to_owned(),
            turn_class: class.to_owned(),
            started_at: started_at.to_owned(),
            calls: 3,
            active_ms,
            explained_ms: 0,
        }
    }

    /// Stores `row` as the rules pass would (`devtime_turn_stats` has no FK, so no session row is
    /// needed).
    pub(crate) async fn insert_stat(pool: &SqlitePool, row: &TurnStatRow) {
        sqlx::query(
            "INSERT INTO devtime_turn_stats (session_id, turn_seq, project_id, turn_class, started_at,
                                             calls, active_ms, explained_ms, rules_version)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, 'v1')",
        )
        .bind(&row.session_id)
        .bind(row.turn_seq)
        .bind(&row.project_id)
        .bind(&row.turn_class)
        .bind(&row.started_at)
        .bind(row.calls)
        .bind(row.active_ms)
        .bind(row.explained_ms)
        .execute(pool)
        .await
        .unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::turn_stat_fixture::*;
    use super::*;

    const S: &str = "2026-10-01T00:00:00.000Z";
    const U: &str = "2026-10-18T00:00:00.000Z";
    const IN: &str = "2026-10-05T10:00:00.000Z";

    async fn pool() -> SqlitePool {
        crate::testdb::fresh_pool().await
    }

    #[tokio::test]
    async fn turn_stat_window_is_half_open_on_started_at() {
        let pool = pool().await;
        let at_since = stat("s1", 1, "alpha", "c1", S, 1_000);
        insert_stat(&pool, &at_since).await;
        insert_stat(&pool, &stat("s1", 2, "alpha", "c1", U, 2_000)).await;
        insert_stat(
            &pool,
            &stat("s1", 0, "alpha", "c1", "2026-09-30T23:59:59.999Z", 3_000),
        )
        .await;

        let rows = turn_stat_window_rows(&pool, None, S, U).await.unwrap();
        assert_eq!(rows, vec![at_since]);
    }

    /// Two stats in `alpha`, one in `beta`, all inside the window.
    async fn two_projects(pool: &SqlitePool) -> (TurnStatRow, TurnStatRow, TurnStatRow) {
        let a1 = stat("a1", 1, "alpha", "c1", IN, 1_000);
        let a2 = stat("a1", 2, "alpha", "c2", "2026-10-06T10:00:00.000Z", 2_000);
        let b1 = stat("b1", 1, "beta", "c1", "2026-10-05T11:00:00.000Z", 3_000);
        for row in [&a1, &a2, &b1] {
            insert_stat(pool, row).await;
        }
        (a1, a2, b1)
    }

    #[tokio::test]
    async fn project_filter_excludes_other_projects() {
        let pool = pool().await;
        let (a1, a2, b1) = two_projects(&pool).await;

        assert_eq!(
            turn_stat_window_rows(&pool, Some("alpha"), S, U)
                .await
                .unwrap(),
            vec![a1, a2]
        );
        assert_eq!(
            turn_stat_window_rows(&pool, Some("beta"), S, U)
                .await
                .unwrap(),
            vec![b1]
        );
    }

    #[tokio::test]
    async fn none_project_sums_all() {
        let pool = pool().await;
        let (a1, a2, b1) = two_projects(&pool).await;

        assert_eq!(
            turn_stat_window_rows(&pool, None, S, U).await.unwrap(),
            vec![a1, b1, a2]
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

    /// Spec §9 (performance): the global form takes its rows from migration 0256's index; the
    /// project form may use either index. Neither scans the table.
    #[tokio::test]
    async fn the_turn_stat_reads_take_rows_from_the_day_index() {
        let pool = pool().await;
        let global = read_plan(&pool, TURN_STAT_WINDOW_SQL, false).await;
        assert!(
            global
                .iter()
                .any(|line| line.contains("idx_devtime_turn_stats_day")),
            "{global:?}"
        );
        for plan in [
            global,
            read_plan(&pool, TURN_STAT_WINDOW_PROJECT_SQL, true).await,
        ] {
            assert!(
                !plan.iter().any(|line| line.starts_with("SCAN ")),
                "{plan:?}"
            );
        }
    }
}
