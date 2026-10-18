//! The analytics reads of the devtime tables (spec 2026-10-17-analytics-devtime §5.1): rows
//! already grouped by UTC day for `analytics/devtime.rs` to assemble. Read-only; the only writer F2
//! reaches is the feedback upsert in `rules.rs`, through `precision::mark_case`.
//!
//! Every function takes `since`/`until` stamps (`%Y-%m-%dT%H:%M:%S%.3fZ`, the format every devtime
//! timestamp is written in, so text order is time order) and an optional project. The project lives
//! on `devtime_sessions` only, so the filter is always a join through it.

use crate::devtime::store::FeedbackRow;
use sqlx::SqlitePool;

/// Milliseconds of span time on one UTC day, for one combination of the columns the page breaks it
/// by.
#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct SpanDayRow {
    pub day: String,
    pub kind: String,
    pub waste: Option<String>,
    pub rule_id: Option<String>,
    pub lever: Option<String>,
    pub confidence: String,
    /// `main` or `other` (every `agent:*` / `bg:*` lane): only `main` is active time (spec §6.3).
    pub lane_class: String,
    pub ms: i64,
}

/// Span time clipped to `[since, until)` and split at each UTC midnight (spec §4.2, §6.2): a span
/// counts on the days it RUNS, so the per-day sum equals the clipped wall-clock length.
///
/// The caller passes `until = min(window end, now)`, which is also what keeps a span whose recorded
/// end is in the future from counting time that has not happened.
pub async fn span_day_rows(
    pool: &SqlitePool,
    project_id: Option<&str>,
    since: &str,
    until: &str,
) -> sqlx::Result<Vec<SpanDayRow>> {
    sqlx::query_as::<_, SpanDayRow>(
        "WITH RECURSIVE days(d) AS (
             SELECT date(?1)
             UNION ALL SELECT date(d, '+1 day') FROM days WHERE date(d, '+1 day') < ?2
         ),
         bounds AS (
             SELECT d, d || 'T00:00:00.000Z' AS d0, date(d, '+1 day') || 'T00:00:00.000Z' AS d1 FROM days
         ),
         clipped AS (
             SELECT sp.kind, sp.waste, sp.rule_id, sp.lever, sp.confidence,
                    CASE WHEN sp.lane = 'main' THEN 'main' ELSE 'other' END AS lane_class,
                    max(sp.started_at, ?1) AS s, min(sp.ended_at, ?2) AS e
             FROM devtime_spans sp
             JOIN devtime_sessions ss ON ss.session_id = sp.session_id
             WHERE (?3 IS NULL OR ss.project_id = ?3)
               AND sp.started_at < ?2 AND sp.ended_at > ?1
         )
         SELECT b.d AS day, c.kind, c.waste, c.rule_id, c.lever, c.confidence, c.lane_class,
                CAST(round(SUM((julianday(min(c.e, b.d1)) - julianday(max(c.s, b.d0))) * 86400000.0))
                     AS INTEGER) AS ms
         FROM clipped c JOIN bounds b ON c.s < b.d1 AND c.e > b.d0
         GROUP BY b.d, c.kind, c.waste, c.rule_id, c.lever, c.confidence, c.lane_class
         ORDER BY b.d",
    )
    .bind(since)
    .bind(until)
    .bind(project_id)
    .fetch_all(pool)
    .await
}

/// Tokens on one UTC day (of `first_at`, no split: a message is an instant), per model and effort.
/// Every lane is summed. `NULL` model/effort stay `None` here; the assembler names them `unknown`.
#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct MessageDayRow {
    pub day: String,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub input: i64,
    pub cache_read: i64,
    pub cache_creation: i64,
    pub output: i64,
    pub messages: i64,
}

pub async fn message_day_rows(
    pool: &SqlitePool,
    project_id: Option<&str>,
    since: &str,
    until: &str,
) -> sqlx::Result<Vec<MessageDayRow>> {
    sqlx::query_as::<_, MessageDayRow>(
        "SELECT date(m.first_at) AS day, m.model, m.effort,
                COALESCE(SUM(COALESCE(m.input_tokens, 0)), 0) AS input,
                COALESCE(SUM(COALESCE(m.cache_read_tokens, 0)), 0) AS cache_read,
                COALESCE(SUM(COALESCE(m.cache_creation_tokens, 0)), 0) AS cache_creation,
                COALESCE(SUM(COALESCE(m.output_tokens, 0)), 0) AS output,
                COUNT(*) AS messages
         FROM devtime_messages m
         JOIN devtime_sessions ss ON ss.session_id = m.session_id
         WHERE (?3 IS NULL OR ss.project_id = ?3) AND m.first_at >= ?1 AND m.first_at < ?2
         GROUP BY day, m.model, m.effort
         ORDER BY day, m.model, m.effort",
    )
    .bind(since)
    .bind(until)
    .bind(project_id)
    .fetch_all(pool)
    .await
}

/// Turns on one UTC day (of `started_at`). `correction_known` counts turns whose
/// `opens_with_correction` is known (spec §6.4's `turns_known`).
#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct TurnDayRow {
    pub day: String,
    pub turns: i64,
    pub interrupted: i64,
    pub corrections: i64,
    pub correction_known: i64,
}

pub async fn turn_day_rows(
    pool: &SqlitePool,
    project_id: Option<&str>,
    since: &str,
    until: &str,
) -> sqlx::Result<Vec<TurnDayRow>> {
    sqlx::query_as::<_, TurnDayRow>(
        "SELECT date(t.started_at) AS day, COUNT(*) AS turns,
                COALESCE(SUM(t.interrupted <> 0), 0) AS interrupted,
                COALESCE(SUM(COALESCE(t.opens_with_correction, 0) <> 0), 0) AS corrections,
                COALESCE(SUM(t.opens_with_correction IS NOT NULL), 0) AS correction_known
         FROM devtime_turns t
         JOIN devtime_sessions ss ON ss.session_id = t.session_id
         WHERE (?3 IS NULL OR ss.project_id = ?3) AND t.started_at >= ?1 AND t.started_at < ?2
         GROUP BY day ORDER BY day",
    )
    .bind(since)
    .bind(until)
    .bind(project_id)
    .fetch_all(pool)
    .await
}

/// The window's sessions for the quality strip: how many overlap it, how many are `dirty`, how many
/// were never ruled (`rules_version` NULL, column of 0171) — their spans count as `useful`
/// (spec §6.6).
#[derive(Debug, Clone, Default, PartialEq, sqlx::FromRow)]
pub struct SessionCounts {
    pub sessions: i64,
    pub dirty: i64,
    pub unruled: i64,
}

pub async fn session_counts(
    pool: &SqlitePool,
    project_id: Option<&str>,
    since: &str,
    until: &str,
) -> sqlx::Result<SessionCounts> {
    sqlx::query_as::<_, SessionCounts>(
        "SELECT COUNT(*) AS sessions,
                COALESCE(SUM(dirty <> 0), 0) AS dirty,
                COALESCE(SUM(rules_version IS NULL), 0) AS unruled
         FROM devtime_sessions
         WHERE (?3 IS NULL OR project_id = ?3)
           AND COALESCE(started_at, updated_at) < ?2
           AND COALESCE(ended_at, updated_at) >= ?1",
    )
    .bind(since)
    .bind(until)
    .bind(project_id)
    .fetch_one(pool)
    .await
}

/// Findings per rule whose `started_at` falls in the window — the `findings` column of
/// `waste_by_rule`.
pub async fn finding_rule_counts(
    pool: &SqlitePool,
    project_id: Option<&str>,
    since: &str,
    until: &str,
) -> sqlx::Result<Vec<(String, i64)>> {
    sqlx::query_as::<_, (String, i64)>(
        "SELECT rule_id, COUNT(*) FROM devtime_findings
         WHERE (?3 IS NULL OR project_id = ?3) AND started_at >= ?1 AND started_at < ?2
         GROUP BY rule_id ORDER BY rule_id",
    )
    .bind(since)
    .bind(until)
    .bind(project_id)
    .fetch_all(pool)
    .await
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FindingOrder {
    /// `cost_ms` descending — a sort weight, never minutes (spec §6.7).
    Cost,
    /// `started_at` descending.
    Recent,
}

/// Where the next page starts: the last row's sort value (as text) and key. Opaque to the client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FindingCursor {
    pub sort: String,
    pub key: String,
}

/// A finding with its standing mark, if any.
#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct FindingItemRow {
    pub finding_key: String,
    pub project_id: String,
    pub session_id: String,
    pub scope: String,
    pub rule_id: String,
    pub level: String,
    pub waste: String,
    pub lever: String,
    pub confidence: String,
    pub started_at: String,
    pub cost_ms: i64,
    pub count: i64,
    pub verdict: Option<String>,
    pub cause: Option<String>,
    pub marked_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FindingsPage {
    pub items: Vec<FindingItemRow>,
    pub next: Option<FindingCursor>,
}

const FINDING_SELECT: &str = "SELECT f.finding_key, f.project_id, f.session_id, f.scope, f.rule_id,
            f.level, f.waste, f.lever, f.confidence, f.started_at, f.cost_ms, f.count,
            b.verdict, b.cause, b.marked_at
     FROM devtime_findings f LEFT JOIN devtime_feedback b ON b.finding_key = f.finding_key
     WHERE (?3 IS NULL OR f.project_id = ?3) AND f.started_at >= ?1 AND f.started_at < ?2
       AND (?4 IS NULL OR f.rule_id = ?4)";

/// One page of findings, keyset-paginated on `(sort value, finding_key)` so a page never repeats or
/// skips a row while marks are written (spec §6.7). Fetches `limit + 1` to know whether more
/// remain.
#[allow(clippy::too_many_arguments)]
pub async fn findings_page(
    pool: &SqlitePool,
    project_id: Option<&str>,
    since: &str,
    until: &str,
    rule_id: Option<&str>,
    order: FindingOrder,
    limit: i64,
    cursor: Option<&FindingCursor>,
) -> sqlx::Result<FindingsPage> {
    let sql = match order {
        FindingOrder::Cost => format!(
            "{FINDING_SELECT}
               AND (?5 IS NULL OR f.cost_ms < CAST(?5 AS INTEGER)
                    OR (f.cost_ms = CAST(?5 AS INTEGER) AND f.finding_key > ?6))
             ORDER BY f.cost_ms DESC, f.finding_key ASC LIMIT ?7"
        ),
        FindingOrder::Recent => format!(
            "{FINDING_SELECT}
               AND (?5 IS NULL OR f.started_at < ?5 OR (f.started_at = ?5 AND f.finding_key > ?6))
             ORDER BY f.started_at DESC, f.finding_key ASC LIMIT ?7"
        ),
    };
    // `sql` interpolates only the constant above; every client value is bound.
    let mut items = sqlx::query_as::<_, FindingItemRow>(sqlx::AssertSqlSafe(sql))
        .bind(since)
        .bind(until)
        .bind(project_id)
        .bind(rule_id)
        .bind(cursor.map(|c| c.sort.as_str()))
        .bind(cursor.map(|c| c.key.as_str()))
        .bind(limit + 1)
        .fetch_all(pool)
        .await?;
    let next = if items.len() as i64 > limit {
        items.truncate(limit as usize);
        items.last().map(|last| FindingCursor {
            sort: match order {
                FindingOrder::Cost => last.cost_ms.to_string(),
                FindingOrder::Recent => last.started_at.clone(),
            },
            key: last.finding_key.clone(),
        })
    } else {
        None
    };
    Ok(FindingsPage { items, next })
}

/// The standing mark on one finding, or `None`.
pub async fn feedback_by_key(pool: &SqlitePool, key: &str) -> sqlx::Result<Option<FeedbackRow>> {
    sqlx::query_as::<_, FeedbackRow>(
        "SELECT finding_key, rule_id, session_id, verdict, cause, rule_version, marked_at
         FROM devtime_feedback WHERE finding_key = ?",
    )
    .bind(key)
    .fetch_optional(pool)
    .await
}

#[cfg(test)]
pub(crate) mod fixture {
    use sqlx::SqlitePool;

    /// A session row; `ended` `None` is a session in progress.
    pub(crate) async fn session(
        pool: &SqlitePool,
        id: &str,
        project: &str,
        started: &str,
        ended: Option<&str>,
    ) {
        sqlx::query(
            "INSERT INTO devtime_sessions (session_id, project_id, started_at, ended_at, dirty,
                                           parser_version, updated_at, rules_version)
             VALUES (?, ?, ?, ?, 0, 1, ?, 'v1')",
        )
        .bind(id)
        .bind(project)
        .bind(started)
        .bind(ended)
        .bind(started)
        .execute(pool)
        .await
        .unwrap();
    }

    pub(crate) struct Span<'a> {
        pub session: &'a str,
        pub lane: &'a str,
        pub kind: &'a str,
        pub start: &'a str,
        pub end: &'a str,
        pub waste: Option<&'a str>,
        pub rule: Option<&'a str>,
        pub lever: Option<&'a str>,
        pub confidence: &'a str,
    }

    /// A `main`-lane `tool` span, exact, no waste — override what a test is about.
    pub(crate) fn span<'a>(session: &'a str, start: &'a str, end: &'a str) -> Span<'a> {
        Span {
            session,
            lane: "main",
            kind: "tool",
            start,
            end,
            waste: None,
            rule: None,
            lever: None,
            confidence: "exact",
        }
    }

    pub(crate) async fn insert_span(pool: &SqlitePool, s: Span<'_>) {
        sqlx::query(
            "INSERT INTO devtime_spans (session_id, lane, kind, started_at, ended_at, waste, rule_id,
                                        lever, confidence, parser_version)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, 1)",
        )
        .bind(s.session)
        .bind(s.lane)
        .bind(s.kind)
        .bind(s.start)
        .bind(s.end)
        .bind(s.waste)
        .bind(s.rule)
        .bind(s.lever)
        .bind(s.confidence)
        .execute(pool)
        .await
        .unwrap();
    }

    pub(crate) async fn message(
        pool: &SqlitePool,
        session: &str,
        lane: &str,
        id: &str,
        at: &str,
        model: Option<&str>,
        tokens: [i64; 4],
    ) {
        sqlx::query(
            "INSERT INTO devtime_messages (session_id, lane, message_id, first_at, last_at, model, effort,
                 input_tokens, cache_read_tokens, cache_creation_tokens, output_tokens, parser_version)
             VALUES (?, ?, ?, ?, ?, ?, NULL, ?, ?, ?, ?, 1)",
        )
        .bind(session)
        .bind(lane)
        .bind(id)
        .bind(at)
        .bind(at)
        .bind(model)
        .bind(tokens[0])
        .bind(tokens[1])
        .bind(tokens[2])
        .bind(tokens[3])
        .execute(pool)
        .await
        .unwrap();
    }

    pub(crate) async fn turn(
        pool: &SqlitePool,
        session: &str,
        seq: i64,
        at: &str,
        interrupted: bool,
        correction: Option<bool>,
    ) {
        sqlx::query(
            "INSERT INTO devtime_turns (session_id, seq, started_at, ended_at, interrupted,
                                        opens_with_correction, parser_version)
             VALUES (?, ?, ?, ?, ?, ?, 1)",
        )
        .bind(session)
        .bind(seq)
        .bind(at)
        .bind(at)
        .bind(i64::from(interrupted))
        .bind(correction.map(i64::from))
        .execute(pool)
        .await
        .unwrap();
    }

    pub(crate) async fn finding(
        pool: &SqlitePool,
        key: &str,
        project: &str,
        rule: &str,
        at: &str,
        cost_ms: i64,
    ) {
        sqlx::query(
            "INSERT INTO devtime_findings (finding_key, project_id, session_id, scope, rule_id, rule_version,
                 level, waste, lever, confidence, lane, started_at, ended_at, cost_ms, count,
                 rules_version, parser_version)
             VALUES (?, ?, 's1', 'session', ?, 1, 'base', 'rework', 'spec_gap', 'inferred', 'main',
                     ?, ?, ?, 1, 'v1', 1)",
        )
        .bind(key)
        .bind(project)
        .bind(rule)
        .bind(at)
        .bind(at)
        .bind(cost_ms)
        .execute(pool)
        .await
        .unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::fixture::*;
    use super::*;

    const MIN: i64 = 60_000;
    const S: &str = "2026-10-01T00:00:00.000Z";
    const U: &str = "2026-10-18T00:00:00.000Z";
    /// Two ids that exist in `devtime::rules::RULES`.
    const RULE: &str = "A1";
    const OTHER_RULE: &str = "A2";

    async fn pool() -> sqlx::SqlitePool {
        crate::testdb::fresh_pool().await
    }

    /// An exact `main`-lane tool span in session `session_id`.
    async fn tool_span(pool: &sqlx::SqlitePool, session_id: &str, start: &str, end: &str) {
        insert_span(pool, span(session_id, start, end)).await;
    }

    fn ms_on(rows: &[SpanDayRow], day: &str) -> i64 {
        rows.iter().filter(|r| r.day == day).map(|r| r.ms).sum()
    }

    /// Spec §9: 23:30 -> 00:45 UTC is 30 minutes on the first day and 45 on the second, 75 in all.
    #[tokio::test]
    async fn span_split_at_midnight_sums_to_clipped_length() {
        let pool = pool().await;
        session(&pool, "s1", "alpha", "2026-10-10T23:00:00.000Z", None).await;
        tool_span(&pool, "s1", "2026-10-10T23:30:00.000Z", "2026-10-11T00:45:00.000Z").await;
        let rows = span_day_rows(&pool, None, S, U).await.unwrap();
        assert_eq!(ms_on(&rows, "2026-10-10"), 30 * MIN);
        assert_eq!(ms_on(&rows, "2026-10-11"), 45 * MIN);
        assert_eq!(rows.iter().map(|r| r.ms).sum::<i64>(), 75 * MIN);
    }

    /// Only the part inside `[since, until)` counts, at both edges.
    #[tokio::test]
    async fn span_clipped_at_window_edges() {
        let pool = pool().await;
        session(&pool, "s1", "alpha", "2026-09-30T22:00:00.000Z", None).await;
        tool_span(&pool, "s1", "2026-09-30T23:00:00.000Z", "2026-10-01T00:20:00.000Z").await;
        tool_span(&pool, "s1", "2026-10-03T11:50:00.000Z", "2026-10-03T12:30:00.000Z").await;
        let rows = span_day_rows(&pool, None, S, "2026-10-03T12:00:00.000Z")
            .await
            .unwrap();
        assert_eq!(ms_on(&rows, "2026-09-30"), 0);
        assert_eq!(ms_on(&rows, "2026-10-01"), 20 * MIN);
        assert_eq!(ms_on(&rows, "2026-10-03"), 10 * MIN);
    }

    /// `lane_class` folds every non-main lane into `other`; vocabulary columns come back as stored.
    #[tokio::test]
    async fn span_rows_carry_lane_class_and_classification() {
        let pool = pool().await;
        session(&pool, "s1", "alpha", "2026-10-05T09:00:00.000Z", None).await;
        let (from, ten, five) = (
            "2026-10-05T09:00:00.000Z",
            "2026-10-05T09:10:00.000Z",
            "2026-10-05T09:05:00.000Z",
        );
        insert_span(
            &pool,
            Span {
                lane: "agent:x",
                ..span("s1", from, ten)
            },
        )
        .await;
        insert_span(
            &pool,
            Span {
                waste: Some("rework"),
                rule: Some("A1"),
                lever: Some("spec_gap"),
                confidence: "inferred",
                ..span("s1", from, five)
            },
        )
        .await;
        let rows = span_day_rows(&pool, None, S, U).await.unwrap();
        assert!(
            rows.iter()
                .any(|r| r.lane_class == "other" && r.ms == 10 * MIN)
        );
        assert!(rows.iter().any(|r| r.lane_class == "main"
            && r.waste.as_deref() == Some("rework")
            && r.rule_id.as_deref() == Some("A1")
            && r.lever.as_deref() == Some("spec_gap")
            && r.confidence == "inferred"
            && r.ms == 5 * MIN));
    }

    #[tokio::test]
    async fn project_filter_excludes_other_projects() {
        let pool = pool().await;
        session(&pool, "a", "alpha", "2026-10-05T09:00:00.000Z", None).await;
        session(&pool, "b", "beta", "2026-10-05T09:00:00.000Z", None).await;
        tool_span(&pool, "a", "2026-10-05T09:00:00.000Z", "2026-10-05T09:01:00.000Z").await;
        tool_span(&pool, "b", "2026-10-05T09:00:00.000Z", "2026-10-05T09:02:00.000Z").await;
        let alpha = span_day_rows(&pool, Some("alpha"), S, U).await.unwrap();
        let all = span_day_rows(&pool, None, S, U).await.unwrap();
        assert_eq!(alpha.iter().map(|r| r.ms).sum::<i64>(), MIN);
        assert_eq!(
            all.iter().map(|r| r.ms).sum::<i64>(),
            3 * MIN,
            "none_project_sums_all"
        );
    }

    /// Every lane's tokens count (subagents spend real tokens), on the day of `first_at`.
    #[tokio::test]
    async fn messages_sum_every_lane_per_day_and_model() {
        let pool = pool().await;
        let opus = Some("claude-opus-4-7");
        session(&pool, "s1", "alpha", "2026-10-05T09:00:00.000Z", None).await;
        let (m1, m2, m3) = (
            "2026-10-05T09:00:00.000Z",
            "2026-10-05T23:59:00.000Z",
            "2026-10-06T00:01:00.000Z",
        );
        message(&pool, "s1", "main", "m1", m1, opus, [10, 20, 30, 40]).await;
        message(&pool, "s1", "agent:x", "m2", m2, opus, [1, 0, 0, 1]).await;
        message(&pool, "s1", "main", "m3", m3, None, [5, 0, 0, 5]).await;
        let rows = message_day_rows(&pool, None, S, U).await.unwrap();
        let day_one = rows.iter().find(|r| r.day == "2026-10-05").unwrap();
        assert_eq!(
            (
                day_one.input,
                day_one.cache_read,
                day_one.cache_creation,
                day_one.output,
                day_one.messages
            ),
            (11, 20, 30, 41, 2)
        );
        let unknown = rows.iter().find(|r| r.day == "2026-10-06").unwrap();
        assert_eq!(
            (unknown.model.as_deref(), unknown.effort.as_deref()),
            (None, None)
        );
    }

    #[tokio::test]
    async fn turns_count_interruptions_and_known_corrections() {
        let pool = pool().await;
        session(&pool, "s1", "alpha", "2026-10-05T09:00:00.000Z", None).await;
        turn(&pool, "s1", 1, "2026-10-05T09:00:00.000Z", true, Some(true)).await;
        turn(&pool, "s1", 2, "2026-10-05T09:10:00.000Z", false, Some(false)).await;
        turn(&pool, "s1", 3, "2026-10-05T09:20:00.000Z", false, None).await;
        let rows = turn_day_rows(&pool, None, S, U).await.unwrap();
        assert_eq!(
            rows,
            vec![TurnDayRow {
                day: "2026-10-05".into(),
                turns: 3,
                interrupted: 1,
                corrections: 1,
                correction_known: 2,
            }]
        );
    }

    /// Sessions overlapping the window; `dirty` and never-ruled counted apart.
    #[tokio::test]
    async fn session_counts_report_dirty_and_unruled() {
        let pool = pool().await;
        let (in_start, in_end) = ("2026-10-05T09:00:00.000Z", "2026-10-05T10:00:00.000Z");
        let (old_start, old_end) = ("2026-08-01T09:00:00.000Z", "2026-08-01T10:00:00.000Z");
        session(&pool, "in", "alpha", in_start, Some(in_end)).await;
        session(&pool, "open", "alpha", "2026-10-06T09:00:00.000Z", None).await;
        session(&pool, "old", "alpha", old_start, Some(old_end)).await;
        sqlx::query(
            "UPDATE devtime_sessions SET dirty = 1, rules_version = NULL WHERE session_id = 'open'",
        )
        .execute(&pool)
        .await
        .unwrap();
        let counts = session_counts(&pool, None, S, U).await.unwrap();
        assert_eq!(
            counts,
            SessionCounts {
                sessions: 2,
                dirty: 1,
                unruled: 1
            }
        );
    }

    /// Five findings, pages of two by cost (a tie on 400): every finding once, in order, then no
    /// cursor; the standing mark joined onto its row.
    #[tokio::test]
    async fn findings_page_joins_feedback_and_paginates_stably() {
        let pool = pool().await;
        for (key, cost) in [("k1", 500), ("k2", 400), ("k3", 400), ("k4", 100), ("k5", 50)] {
            finding(&pool, key, "alpha", RULE, "2026-10-05T09:00:00.000Z", cost).await;
        }
        crate::devtime::precision::mark_case(&pool, "k2", "not_rework", None)
            .await
            .unwrap();
        let mut seen = Vec::new();
        let mut cursor = None;
        loop {
            let order = FindingOrder::Cost;
            let page = findings_page(&pool, None, S, U, None, order, 2, cursor.as_ref())
                .await
                .unwrap();
            seen.extend(page.items.iter().map(|row| row.finding_key.clone()));
            if let Some(row) = page.items.iter().find(|row| row.finding_key == "k2") {
                assert_eq!(row.verdict.as_deref(), Some("not_rework"));
            }
            match page.next {
                Some(next) => cursor = Some(next),
                None => break,
            }
        }
        assert_eq!(seen, ["k1", "k2", "k3", "k4", "k5"]);
    }

    #[tokio::test]
    async fn findings_page_filters_by_rule_project_and_window_and_orders_by_recent() {
        let pool = pool().await;
        finding(&pool, "old", "alpha", RULE, "2026-09-01T09:00:00.000Z", 900).await;
        finding(&pool, "a", "alpha", RULE, "2026-10-05T09:00:00.000Z", 1).await;
        finding(&pool, "b", "alpha", OTHER_RULE, "2026-10-06T09:00:00.000Z", 1).await;
        finding(&pool, "c", "beta", RULE, "2026-10-07T09:00:00.000Z", 1).await;
        let recent = findings_page(&pool, None, S, U, None, FindingOrder::Recent, 50, None)
            .await
            .unwrap();
        let keys: Vec<&str> = recent.items.iter().map(|r| r.finding_key.as_str()).collect();
        assert_eq!(keys, ["c", "b", "a"]);
        let order = FindingOrder::Cost;
        let filtered = findings_page(&pool, Some("alpha"), S, U, Some(RULE), order, 50, None)
            .await
            .unwrap();
        assert_eq!(filtered.items.len(), 1);
        let counts = finding_rule_counts(&pool, Some("alpha"), S, U)
            .await
            .unwrap();
        assert_eq!(counts.len(), 2);
        assert!(counts.iter().all(|(_, n)| *n == 1));
    }

    #[tokio::test]
    async fn feedback_by_key_reads_the_standing_mark() {
        let pool = pool().await;
        finding(&pool, "k1", "alpha", RULE, "2026-10-05T09:00:00.000Z", 1).await;
        assert!(feedback_by_key(&pool, "k1").await.unwrap().is_none());
        crate::devtime::precision::mark_case(&pool, "k1", "confirmed", Some("spec_gap"))
            .await
            .unwrap();
        let mark = feedback_by_key(&pool, "k1").await.unwrap().unwrap();
        assert_eq!(
            (mark.verdict.as_str(), mark.cause.as_deref()),
            ("confirmed", Some("spec_gap"))
        );
    }
}
