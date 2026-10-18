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

/// One read in its two forms (spec §6.5): `$global`, and `$project` with `$filter` spliced in after
/// the window. One `(?3 IS NULL OR ss.project_id = ?3)` form was what these reads had, and EXPLAIN
/// QUERY PLAN showed it `SCAN sp` for a single project too — SQLite plans the statement once, for
/// either value of `?3`, so it cannot start from an index on `project_id`. The project form starts
/// from `idx_devtime_sessions_project` (migration 0210) and reaches the rows by session; the
/// global form names no `?3`, and is the same scan it always was.
///
/// The span and message reads are the exception: they walk the day indexes of migration 0211 in
/// GROUP BY order (`CROSS JOIN` keeps the devtime table outermost) and check the project per row,
/// so a project costs at most what the global read does. Through the project index the rows came
/// out in session order and were sorted before grouping — 590 ms against 180 ms for the project
/// that holds 98% of the real database's spans. The four-literal arm splices `$filter` in twice,
/// for reads with two passes over the table.
macro_rules! global_and_project {
    ($global:ident, $project:ident, $head:literal, $filter:literal, $tail:literal $(,)?) => {
        pub(crate) const $global: &str = concat!($head, $tail);
        pub(crate) const $project: &str = concat!($head, $filter, $tail);
    };
    (
        $global:ident,
        $project:ident,
        $head:literal,
        $filter:literal,
        $mid:literal,
        $tail:literal $(,)?
    ) => {
        pub(crate) const $global: &str = concat!($head, $mid, $tail);
        pub(crate) const $project: &str = concat!($head, $filter, $mid, $filter, $tail);
    };
}

// Span time clipped to `[?1, ?2)` and split at each UTC midnight — see `span_day_rows`.
//
// Two passes, because joining every span to a recursive list of the window's days made SQLite pair
// each span with each day (50,000 spans x 180 days took 20 s in the debug build). A span that
// starts and ends on one day is its own only piece: `one_day` reads those from
// `idx_devtime_spans_day` already in GROUP BY order, so they are summed without a sort. The few
// that cross a midnight (295 of 210,000 on the real database) come from
// `idx_devtime_spans_multiday` and are cut one day at a time by `pieces`. Each piece is the old
// `(min(e, d1) - max(s, d0))` of its day, and `substr(.., 1, 10)` is `date()` for the one stamp
// format written (module doc). The `BETWEEN` repeats what the window already implies, so the index
// is read from the window's first day instead of from the beginning.
global_and_project!(
    SPAN_DAY_SQL,
    SPAN_DAY_PROJECT_SQL,
    "WITH RECURSIVE one_day AS (
         SELECT substr(sp.started_at, 1, 10) AS d, sp.kind, sp.waste, sp.rule_id, sp.lever,
                sp.confidence,
                CASE WHEN sp.lane = 'main' THEN 'main' ELSE 'other' END AS lane_class,
                SUM((julianday(min(sp.ended_at, ?2)) - julianday(max(sp.started_at, ?1)))
                    * 86400000.0) AS ms
         FROM devtime_spans sp
         CROSS JOIN devtime_sessions ss ON ss.session_id = sp.session_id
         WHERE substr(sp.started_at, 1, 10) = substr(sp.ended_at, 1, 10)
           AND substr(sp.started_at, 1, 10) BETWEEN substr(?1, 1, 10) AND substr(?2, 1, 10)
           AND sp.started_at < ?2 AND sp.ended_at > ?1
           AND min(sp.ended_at, ?2) > substr(sp.started_at, 1, 10) || 'T00:00:00.000Z'",
    " AND ss.project_id = ?3",
    "
         GROUP BY 1, 2, 3, 4, 5, 6, 7
     ),
     clipped AS (
         SELECT sp.kind, sp.waste, sp.rule_id, sp.lever, sp.confidence,
                CASE WHEN sp.lane = 'main' THEN 'main' ELSE 'other' END AS lane_class,
                max(sp.started_at, ?1) AS s, min(sp.ended_at, ?2) AS e
         FROM devtime_spans sp
         CROSS JOIN devtime_sessions ss ON ss.session_id = sp.session_id
         WHERE substr(sp.started_at, 1, 10) <> substr(sp.ended_at, 1, 10)
           AND sp.started_at < ?2 AND sp.ended_at > ?1",
    "
     ),
     pieces(kind, waste, rule_id, lever, confidence, lane_class, d, s, e) AS (
         SELECT kind, waste, rule_id, lever, confidence, lane_class, date(s), s, e
         FROM clipped WHERE e > date(s) || 'T00:00:00.000Z'
         UNION ALL
         SELECT kind, waste, rule_id, lever, confidence, lane_class, date(d, '+1 day'),
                date(d, '+1 day') || 'T00:00:00.000Z', e
         FROM pieces WHERE e > date(d, '+1 day') || 'T00:00:00.000Z'
     ),
     parts AS (
         SELECT d, kind, waste, rule_id, lever, confidence, lane_class, ms FROM one_day
         UNION ALL
         SELECT d, kind, waste, rule_id, lever, confidence, lane_class,
                (julianday(min(e, date(d, '+1 day') || 'T00:00:00.000Z')) - julianday(s))
                    * 86400000.0
         FROM pieces
     )
     SELECT d AS day, kind, waste, rule_id, lever, confidence, lane_class,
            CAST(round(SUM(ms)) AS INTEGER) AS ms
     FROM parts
     GROUP BY d, kind, waste, rule_id, lever, confidence, lane_class
     ORDER BY d",
);

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
    let sql = match project_id {
        Some(_) => SPAN_DAY_PROJECT_SQL,
        None => SPAN_DAY_SQL,
    };
    let query = sqlx::query_as::<_, SpanDayRow>(sql).bind(since);
    match project_id {
        Some(project) => query.bind(until).bind(project).fetch_all(pool).await,
        None => query.bind(until).fetch_all(pool).await,
    }
}

/// Tokens on one UTC day (of `first_at`, no split: a message is an instant), per model, effort,
/// billing mode and prompt-length bucket. Every lane is summed. `NULL` model/effort stay `None`
/// here; the assembler names them `unknown`. `NULL` speed/geo are unknown (rows before 0214).
#[derive(Debug, Clone, Default, PartialEq, sqlx::FromRow)]
pub struct MessageDayRow {
    pub day: String,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub speed: Option<String>,
    pub inference_geo: Option<String>,
    /// How many of the read's prompt thresholds every message of the row reaches, a prompt being
    /// one request's input + cache reads + cache writes: 0 below the first, `k` from the `k`-th on.
    pub bucket: i64,
    pub input: i64,
    pub cache_read: i64,
    /// Every cache write, of either duration.
    pub cache_creation: i64,
    /// Of `cache_creation`, the 1-hour writes of messages whose split is known.
    pub cache_creation_1h: i64,
    /// Of `cache_creation`, the writes of messages whose split is unknown (NULL).
    pub cache_creation_unknown: i64,
    pub output: i64,
    pub messages: i64,
}

/// One request's prompt length, the quantity a price tier keys on.
const MESSAGE_PROMPT: &str = "(m.input_tokens + m.cache_read_tokens + m.cache_creation_tokens)";

/// The day read for `thresholds` prompt thresholds (bound ascending after the window and the
/// project: `?3..` globally, `?4..` for a project).
///
/// Prices stay out of SQL, but a tier is chosen per request, so the sums cannot cross a threshold.
/// One arm per bucket, `UNION ALL`: a bucket in the GROUP BY would sort every message of the window,
/// because no index can hold an expression over bound values, whereas each arm takes its rows from
/// `idx_devtime_messages_day` (0214) in its GROUP BY order, covered, with its prompt range checked
/// per row. That costs one walk of the window's index entries per bucket, and the price table has a
/// handful of distinct thresholds. Rows come in no particular order; the `BETWEEN` is implied by the
/// window, as in `one_day` above.
pub(crate) fn message_day_sql(project: bool, thresholds: usize) -> String {
    let first = if project { 4 } else { 3 };
    let filter = if project { " AND ss.project_id = ?3" } else { "" };
    let arms: Vec<String> = (0..=thresholds)
        .map(|bucket| {
            let mut range = String::new();
            if bucket > 0 {
                range.push_str(&format!(
                    " AND {MESSAGE_PROMPT} >= ?{}",
                    first + bucket - 1
                ));
            }
            if bucket < thresholds {
                range.push_str(&format!(" AND {MESSAGE_PROMPT} < ?{}", first + bucket));
            }
            format!(
                "SELECT substr(m.first_at, 1, 10) AS day, m.model, m.effort, m.speed,
                        m.inference_geo, {bucket} AS bucket,
                        COALESCE(SUM(m.input_tokens), 0) AS input,
                        COALESCE(SUM(m.cache_read_tokens), 0) AS cache_read,
                        COALESCE(SUM(m.cache_creation_tokens), 0) AS cache_creation,
                        COALESCE(SUM(COALESCE(
                            MIN(m.cache_creation_1h_tokens, m.cache_creation_tokens), 0)), 0)
                            AS cache_creation_1h,
                        COALESCE(SUM(CASE WHEN m.cache_creation_1h_tokens IS NULL
                            THEN m.cache_creation_tokens ELSE 0 END), 0) AS cache_creation_unknown,
                        COALESCE(SUM(m.output_tokens), 0) AS output,
                        COUNT(*) AS messages
                 FROM devtime_messages m
                 CROSS JOIN devtime_sessions ss ON ss.session_id = m.session_id
                 WHERE substr(m.first_at, 1, 10) BETWEEN substr(?1, 1, 10) AND substr(?2, 1, 10)
                   AND m.first_at >= ?1 AND m.first_at < ?2{filter}{range}
                 GROUP BY day, m.model, m.effort, m.speed, m.inference_geo"
            )
        })
        .collect();
    arms.join("\n UNION ALL\n ")
}

/// `thresholds` ascending and distinct; a row's `bucket` indexes them (see [`MessageDayRow`]).
pub async fn message_day_rows(
    pool: &SqlitePool,
    project_id: Option<&str>,
    since: &str,
    until: &str,
    thresholds: &[u64],
) -> sqlx::Result<Vec<MessageDayRow>> {
    debug_assert!(thresholds.windows(2).all(|pair| pair[0] < pair[1]));
    let sql = message_day_sql(project_id.is_some(), thresholds.len());
    let mut query = sqlx::query_as::<_, MessageDayRow>(sqlx::AssertSqlSafe(sql))
        .bind(since)
        .bind(until);
    if let Some(project) = project_id {
        query = query.bind(project);
    }
    for threshold in thresholds {
        query = query.bind(i64::try_from(*threshold).unwrap_or(i64::MAX));
    }
    query.fetch_all(pool).await
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

global_and_project!(
    TURN_DAY_SQL,
    TURN_DAY_PROJECT_SQL,
    "SELECT date(t.started_at) AS day, COUNT(*) AS turns,
            COALESCE(SUM(t.interrupted <> 0), 0) AS interrupted,
            COALESCE(SUM(COALESCE(t.opens_with_correction, 0) <> 0), 0) AS corrections,
            COALESCE(SUM(t.opens_with_correction IS NOT NULL), 0) AS correction_known
     FROM devtime_turns t
     JOIN devtime_sessions ss ON ss.session_id = t.session_id
     WHERE t.started_at >= ?1 AND t.started_at < ?2",
    " AND ss.project_id = ?3",
    "
     GROUP BY day ORDER BY day",
);

pub async fn turn_day_rows(
    pool: &SqlitePool,
    project_id: Option<&str>,
    since: &str,
    until: &str,
) -> sqlx::Result<Vec<TurnDayRow>> {
    let sql = match project_id {
        Some(_) => TURN_DAY_PROJECT_SQL,
        None => TURN_DAY_SQL,
    };
    let query = sqlx::query_as::<_, TurnDayRow>(sql).bind(since);
    match project_id {
        Some(project) => query.bind(until).bind(project).fetch_all(pool).await,
        None => query.bind(until).fetch_all(pool).await,
    }
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

global_and_project!(
    SESSION_COUNTS_SQL,
    SESSION_COUNTS_PROJECT_SQL,
    "SELECT COUNT(*) AS sessions,
            COALESCE(SUM(dirty <> 0), 0) AS dirty,
            COALESCE(SUM(rules_version IS NULL), 0) AS unruled
     FROM devtime_sessions
     WHERE COALESCE(started_at, updated_at) < ?2
       AND COALESCE(ended_at, updated_at) >= ?1",
    " AND project_id = ?3",
    "",
);

pub async fn session_counts(
    pool: &SqlitePool,
    project_id: Option<&str>,
    since: &str,
    until: &str,
) -> sqlx::Result<SessionCounts> {
    let sql = match project_id {
        Some(_) => SESSION_COUNTS_PROJECT_SQL,
        None => SESSION_COUNTS_SQL,
    };
    let query = sqlx::query_as::<_, SessionCounts>(sql).bind(since);
    match project_id {
        Some(project) => query.bind(until).bind(project).fetch_one(pool).await,
        None => query.bind(until).fetch_one(pool).await,
    }
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

    /// A main-lane message with its billing mode: `one_hour` of its cache writes `[2]` were 1-hour
    /// (`None` unknown), `speed` and `geo` as the transcript said them.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn billed_message(
        pool: &SqlitePool,
        session: &str,
        id: &str,
        at: &str,
        model: &str,
        tokens: [i64; 4],
        one_hour: Option<i64>,
        (speed, geo): (Option<&str>, Option<&str>),
    ) {
        message(pool, session, "main", id, at, Some(model), tokens).await;
        sqlx::query(
            "UPDATE devtime_messages SET cache_creation_1h_tokens = ?, speed = ?, inference_geo = ?
             WHERE session_id = ? AND message_id = ?",
        )
        .bind(one_hour)
        .bind(speed)
        .bind(geo)
        .bind(session)
        .bind(id)
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
        tool_span(
            &pool,
            "s1",
            "2026-10-10T23:30:00.000Z",
            "2026-10-11T00:45:00.000Z",
        )
        .await;
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
        tool_span(
            &pool,
            "s1",
            "2026-09-30T23:00:00.000Z",
            "2026-10-01T00:20:00.000Z",
        )
        .await;
        tool_span(
            &pool,
            "s1",
            "2026-10-03T11:50:00.000Z",
            "2026-10-03T12:30:00.000Z",
        )
        .await;
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
        tool_span(
            &pool,
            "a",
            "2026-10-05T09:00:00.000Z",
            "2026-10-05T09:01:00.000Z",
        )
        .await;
        tool_span(
            &pool,
            "b",
            "2026-10-05T09:00:00.000Z",
            "2026-10-05T09:02:00.000Z",
        )
        .await;
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
        let rows = message_day_rows(&pool, None, S, U, &[]).await.unwrap();
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
        assert_eq!(
            (
                day_one.cache_creation_1h,
                day_one.cache_creation_unknown,
                day_one.bucket
            ),
            (0, 30, 0),
            "a message ingested before 0214 has an unknown cache split"
        );
        let unknown = rows.iter().find(|r| r.day == "2026-10-06").unwrap();
        assert_eq!(
            (unknown.model.as_deref(), unknown.effort.as_deref()),
            (None, None)
        );
    }

    /// A message's prompt (input + cache reads + cache writes) puts it in the bucket of the
    /// thresholds it reaches, inclusive; billing mode and the known 1-hour writes are kept apart,
    /// and no message is lost or counted twice across the arms.
    #[tokio::test]
    async fn messages_split_by_prompt_bucket_and_billing_mode() {
        let pool = pool().await;
        session(&pool, "s1", "alpha", "2026-10-05T09:00:00.000Z", None).await;
        let at = |minute: u32| format!("2026-10-05T09:{minute:02}:00.000Z");
        let haiku = "claude-haiku-5-5";
        let standard = (Some("standard"), Some("global"));
        // Prompts: 99,999 | 100,000 (reaches the first, inclusive) | 250,000 | 100,000 fast.
        let fast_us = (Some("fast"), Some("us"));
        let messages = [
            ("a", 1, [99_999, 0, 0, 1], Some(0), standard),
            ("b", 2, [50_000, 40_000, 10_000, 2], Some(6_000), standard),
            ("c", 3, [250_000, 0, 0, 3], Some(0), standard),
            ("d", 4, [90_000, 0, 10_000, 4], None, fast_us),
        ];
        for (id, minute, tokens, one_hour, mode) in messages {
            billed_message(&pool, "s1", id, &at(minute), haiku, tokens, one_hour, mode).await;
        }
        let mut rows = message_day_rows(&pool, None, S, U, &[100_000, 200_000])
            .await
            .unwrap();
        rows.sort_by_key(|row| (row.bucket, row.speed.clone()));
        let summary: Vec<_> = rows
            .iter()
            .map(|row| {
                (
                    row.bucket,
                    row.speed.as_deref(),
                    row.inference_geo.as_deref(),
                    row.output,
                    row.cache_creation,
                    row.cache_creation_1h,
                    row.cache_creation_unknown,
                )
            })
            .collect();
        assert_eq!(
            summary,
            [
                (0, Some("standard"), Some("global"), 1, 0, 0, 0),
                (1, Some("fast"), Some("us"), 4, 10_000, 0, 10_000),
                (1, Some("standard"), Some("global"), 2, 10_000, 6_000, 0),
                (2, Some("standard"), Some("global"), 3, 0, 0, 0),
            ]
        );
        let project = message_day_rows(&pool, Some("alpha"), S, U, &[100_000, 200_000])
            .await
            .unwrap();
        assert_eq!(
            project.len(),
            rows.len(),
            "the project form binds the same thresholds"
        );
        assert_eq!(project.iter().map(|row| row.messages).sum::<i64>(), 4);
    }

    #[tokio::test]
    async fn turns_count_interruptions_and_known_corrections() {
        let pool = pool().await;
        session(&pool, "s1", "alpha", "2026-10-05T09:00:00.000Z", None).await;
        turn(&pool, "s1", 1, "2026-10-05T09:00:00.000Z", true, Some(true)).await;
        turn(
            &pool,
            "s1",
            2,
            "2026-10-05T09:10:00.000Z",
            false,
            Some(false),
        )
        .await;
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
        for (key, cost) in [
            ("k1", 500),
            ("k2", 400),
            ("k3", 400),
            ("k4", 100),
            ("k5", 50),
        ] {
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
        finding(
            &pool,
            "b",
            "alpha",
            OTHER_RULE,
            "2026-10-06T09:00:00.000Z",
            1,
        )
        .await;
        finding(&pool, "c", "beta", RULE, "2026-10-07T09:00:00.000Z", 1).await;
        let recent = findings_page(&pool, None, S, U, None, FindingOrder::Recent, 50, None)
            .await
            .unwrap();
        let keys: Vec<&str> = recent
            .items
            .iter()
            .map(|r| r.finding_key.as_str())
            .collect();
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
